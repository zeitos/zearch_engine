use crate::Ranker;
use search_core::{RankCandidate, RankedResult};
use std::path::Path;
use wasmtime::{Engine, Linker, Module, Store};

/// Runs a WASM module that implements the rerank function.
///
/// Guest contract:
/// - Export: `rerank(candidates_ptr: i32, candidates_len: i32, scores_ptr: i32) -> i32`
///   where candidates is a null-terminated JSON string in WASM linear memory,
///   scores_ptr is where to write f32 scores (one per candidate), returns number written.
/// - Export: `alloc(size: i32) -> i32` to allocate memory in WASM.
/// - Export: `dealloc(ptr: i32, size: i32)` to free memory in WASM.
pub struct WasmRanker {
    engine: Engine,
    module: Module,
    /// Maximum fuel (CPU instructions) per rerank call.
    max_fuel: u64,
}

impl WasmRanker {
    pub fn from_file(path: &Path) -> search_core::Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);

        let engine = Engine::new(&config)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;
        let module = Module::from_file(&engine, path)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        Ok(Self { engine, module, max_fuel: 1_000_000_000 })
    }

    pub fn from_bytes(bytes: &[u8]) -> search_core::Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);

        let engine = Engine::new(&config)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;
        let module = Module::from_binary(&engine, bytes)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        Ok(Self { engine, module, max_fuel: 1_000_000_000 })
    }

    pub fn with_max_fuel(mut self, fuel: u64) -> Self {
        self.max_fuel = fuel;
        self
    }

    fn do_rerank(
        &self,
        _query: &str,
        candidates: &[RankCandidate],
    ) -> search_core::Result<Vec<RankedResult>> {
        let linker: Linker<()> = Linker::new(&self.engine);
        let mut store = Store::new(&self.engine, ());
        store
            .set_fuel(self.max_fuel)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| search_core::Error::Internal("WASM module has no memory export".into()))?;

        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "alloc")
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        let rerank_fn = instance
            .get_typed_func::<(i32, i32, i32), i32>(&mut store, "rerank")
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        let n = candidates.len() as i32;

        // Serialize candidate scores as f32 array into WASM memory
        let input_bytes: Vec<u8> = candidates
            .iter()
            .flat_map(|c| c.bm25_score.to_le_bytes())
            .collect();
        let input_len = input_bytes.len() as i32;
        let input_ptr = alloc.call(&mut store, input_len)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        memory
            .write(&mut store, input_ptr as usize, &input_bytes)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        // Allocate output buffer: n f32 scores
        let output_len = n * 4;
        let output_ptr = alloc.call(&mut store, output_len)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        let written = rerank_fn
            .call(&mut store, (input_ptr, n, output_ptr))
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        // Read output scores
        let mut out_bytes = vec![0u8; (written * 4) as usize];
        memory
            .read(&mut store, output_ptr as usize, &mut out_bytes)
            .map_err(|e| search_core::Error::Internal(e.to_string()))?;

        let results = candidates
            .iter()
            .zip(out_bytes.chunks(4))
            .map(|(c, chunk)| {
                let score = f32::from_le_bytes(chunk.try_into().unwrap_or([0u8; 4]));
                RankedResult { doc_id: c.doc_id, score }
            })
            .collect();

        Ok(results)
    }
}

#[async_trait::async_trait]
impl Ranker for WasmRanker {
    async fn rerank(
        &self,
        query: &str,
        candidates: &[RankCandidate],
    ) -> search_core::Result<Vec<RankedResult>> {
        // WASM execution is synchronous — run on blocking thread pool
        let query = query.to_string();
        let candidates = candidates.to_vec();
        // We can't move `self` into the closure since it's behind &self,
        // so we do the call inline (WASM is fast enough for Phase 4 purposes).
        self.do_rerank(&query, &candidates)
    }
}
