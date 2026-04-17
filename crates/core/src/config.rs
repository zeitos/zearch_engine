use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level configuration for the search engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_mode")]
    pub mode: Mode,
    #[serde(default)]
    pub router: RouterConfig,
    #[serde(default)]
    pub shard: ShardConfig,
    #[serde(default)]
    pub ranker: RankerConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            router: RouterConfig::default(),
            shard: ShardConfig::default(),
            ranker: RankerConfig::default(),
        }
    }
}

fn default_mode() -> Mode {
    Mode::Standalone
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Router,
    Shard,
    Standalone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default)]
    pub shard_endpoints: Vec<String>,
    #[serde(default = "default_discovery")]
    pub shard_discovery: ShardDiscovery,
    #[serde(default)]
    pub dns_service: Option<String>,
    #[serde(default = "default_query_timeout_ms")]
    pub query_timeout_ms: u64,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            http_port: default_http_port(),
            shard_endpoints: Vec::new(),
            shard_discovery: default_discovery(),
            dns_service: None,
            query_timeout_ms: default_query_timeout_ms(),
        }
    }
}

fn default_http_port() -> u16 {
    8080
}

fn default_discovery() -> ShardDiscovery {
    ShardDiscovery::Static
}

fn default_query_timeout_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShardDiscovery {
    Static,
    Dns,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardConfig {
    #[serde(default = "default_grpc_port")]
    pub grpc_port: u16,
    #[serde(default)]
    pub shard_id: u32,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_num_shards")]
    pub num_shards: u32,
    #[serde(default = "default_write_buffer_size")]
    pub write_buffer_size: usize,
    #[serde(default = "default_merge_threads")]
    pub merge_threads: usize,
}

impl Default for ShardConfig {
    fn default() -> Self {
        Self {
            grpc_port: default_grpc_port(),
            shard_id: 0,
            data_dir: default_data_dir(),
            num_shards: default_num_shards(),
            write_buffer_size: default_write_buffer_size(),
            merge_threads: default_merge_threads(),
        }
    }
}

fn default_grpc_port() -> u16 {
    9001
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}

fn default_num_shards() -> u32 {
    4
}

fn default_write_buffer_size() -> usize {
    64 * 1024 * 1024 // 64MB
}

fn default_merge_threads() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankerConfig {
    #[serde(default = "default_ranker_type")]
    pub r#type: RankerType,
    #[serde(default)]
    pub grpc_endpoint: Option<String>,
    #[serde(default)]
    pub wasm_module: Option<PathBuf>,
    #[serde(default = "default_ranker_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_ranker_candidates")]
    pub candidates: usize,
}

impl Default for RankerConfig {
    fn default() -> Self {
        Self {
            r#type: default_ranker_type(),
            grpc_endpoint: None,
            wasm_module: None,
            timeout_ms: default_ranker_timeout_ms(),
            candidates: default_ranker_candidates(),
        }
    }
}

fn default_ranker_type() -> RankerType {
    RankerType::None
}

fn default_ranker_timeout_ms() -> u64 {
    30
}

fn default_ranker_candidates() -> usize {
    200
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RankerType {
    None,
    Grpc,
    Wasm,
}

impl Config {
    /// Load config from a YAML file, falling back to defaults.
    pub fn from_file(path: &std::path::Path) -> crate::Result<Self> {
        let contents = std::fs::read_to_string(path).map_err(crate::Error::Io)?;
        serde_yaml::from_str(&contents)
            .map_err(|e| crate::Error::Config(e.to_string()))
    }
}
