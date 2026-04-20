use crate::{BreakerRanker, GrpcRanker, NoopRanker, Ranker, WasmRanker};
use search_core::{RankerConfig, RankerType};
use std::sync::Arc;

pub struct RankerFactory;

impl RankerFactory {
    /// Build the ranker from config. Wraps remote rankers in a circuit breaker
    /// when `circuit_breaker.enabled = true`, so a CPU-saturated or down ranker
    /// short-circuits to BM25 fallback instead of stalling search.
    pub fn build(config: &RankerConfig) -> search_core::Result<Arc<dyn Ranker>> {
        let inner: Arc<dyn Ranker> = match config.r#type {
            RankerType::None => Arc::new(NoopRanker),
            RankerType::Grpc => {
                let endpoint = config.grpc_endpoint.clone().ok_or_else(|| {
                    search_core::Error::Config(
                        "grpc ranker requires grpc_endpoint to be set".into(),
                    )
                })?;
                Arc::new(GrpcRanker::new(endpoint))
            }
            RankerType::Wasm => {
                let path = config.wasm_module.as_ref().ok_or_else(|| {
                    search_core::Error::Config(
                        "wasm ranker requires wasm_module path to be set".into(),
                    )
                })?;
                Arc::new(WasmRanker::from_file(path)?)
            }
        };

        if config.circuit_breaker.enabled && matches!(config.r#type, RankerType::Grpc) {
            Ok(Arc::new(BreakerRanker::new(inner, &config.circuit_breaker)))
        } else {
            Ok(inner)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{RankerConfig, RankerType};

    #[test]
    fn test_factory_none() {
        let config = RankerConfig { r#type: RankerType::None, ..Default::default() };
        let ranker = RankerFactory::build(&config);
        assert!(ranker.is_ok());
    }

    #[test]
    fn test_factory_grpc_requires_endpoint() {
        let config = RankerConfig {
            r#type: RankerType::Grpc,
            grpc_endpoint: None,
            ..Default::default()
        };
        assert!(RankerFactory::build(&config).is_err());
    }

    #[test]
    fn test_factory_grpc_with_endpoint() {
        let config = RankerConfig {
            r#type: RankerType::Grpc,
            grpc_endpoint: Some("http://localhost:50051".into()),
            ..Default::default()
        };
        assert!(RankerFactory::build(&config).is_ok());
    }

    #[test]
    fn test_factory_wasm_requires_path() {
        let config = RankerConfig {
            r#type: RankerType::Wasm,
            wasm_module: None,
            ..Default::default()
        };
        assert!(RankerFactory::build(&config).is_err());
    }
}
