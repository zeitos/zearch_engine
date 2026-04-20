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
    pub suggest_shard: SuggestShardConfig,
    #[serde(default)]
    pub ranker: RankerConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            router: RouterConfig::default(),
            shard: ShardConfig::default(),
            suggest_shard: SuggestShardConfig::default(),
            ranker: RankerConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Telemetry config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Enable OpenTelemetry tracing. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// OTLP gRPC endpoint (e.g. "http://otel-collector:4317"). Default: empty.
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,
    /// Service name reported in traces. Default: "search-engine".
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// Fraction of traces to sample (0.0–1.0). Default: 1.0 (sample all).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: default_otlp_endpoint(),
            service_name: default_service_name(),
            sample_rate: default_sample_rate(),
        }
    }
}

fn default_otlp_endpoint() -> String { "http://otel-collector:4317".into() }
fn default_service_name() -> String { "search-engine".into() }
fn default_sample_rate() -> f64 { 1.0 }

fn default_mode() -> Mode {
    Mode::Standalone
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Router,
    Shard,
    Standalone,
    Suggest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// Simple list of shard endpoints (no replicas). Superseded by `shards`.
    #[serde(default)]
    pub shard_endpoints: Vec<String>,
    /// Shard groups with primary + replicas. Takes priority over `shard_endpoints`.
    #[serde(default)]
    pub shards: Vec<ShardGroupConfig>,
    #[serde(default = "default_discovery")]
    pub shard_discovery: ShardDiscovery,
    #[serde(default)]
    pub dns_service: Option<String>,
    #[serde(default = "default_query_timeout_ms")]
    pub query_timeout_ms: u64,
    #[serde(default)]
    pub availability: AvailabilityConfig,
    #[serde(default)]
    pub cache: QueryCacheConfig,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    /// gRPC endpoints of independent suggest shards. If empty, suggest is disabled.
    #[serde(default)]
    pub suggest_shards: Vec<String>,
    /// Timeout for fire-and-forget suggest writes. Default: 200ms.
    #[serde(default = "default_suggest_write_timeout_ms")]
    pub suggest_write_timeout_ms: u64,
    /// Timeout for suggest query fan-out. Default: 100ms.
    #[serde(default = "default_suggest_query_timeout_ms")]
    pub suggest_query_timeout_ms: u64,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            http_port: default_http_port(),
            shard_endpoints: Vec::new(),
            shards: Vec::new(),
            shard_discovery: default_discovery(),
            dns_service: None,
            query_timeout_ms: default_query_timeout_ms(),
            availability: AvailabilityConfig::default(),
            cache: QueryCacheConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            suggest_shards: Vec::new(),
            suggest_write_timeout_ms: default_suggest_write_timeout_ms(),
            suggest_query_timeout_ms: default_suggest_query_timeout_ms(),
        }
    }
}

fn default_suggest_write_timeout_ms() -> u64 { 200 }
fn default_suggest_query_timeout_ms() -> u64 { 100 }

// ---------------------------------------------------------------------------
// Circuit breaker config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    /// Enable per-shard circuit breaker. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Consecutive failures before opening the circuit. Default: 5.
    #[serde(default = "default_cb_threshold")]
    pub failure_threshold: u32,
    /// Milliseconds to wait before probing a half-open circuit. Default: 10_000.
    #[serde(default = "default_cb_recovery_ms")]
    pub recovery_ms: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            failure_threshold: default_cb_threshold(),
            recovery_ms: default_cb_recovery_ms(),
        }
    }
}

fn default_cb_threshold() -> u32 { 5 }
fn default_cb_recovery_ms() -> u64 { 10_000 }

// ---------------------------------------------------------------------------
// Query cache config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryCacheConfig {
    /// Enable the in-process query result cache. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum number of cache entries. Default: 10_000.
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: u64,
    /// Time-to-live per entry in seconds. Default: 60.
    #[serde(default = "default_cache_ttl_seconds")]
    pub ttl_seconds: u64,
}

impl Default for QueryCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_entries: default_cache_max_entries(),
            ttl_seconds: default_cache_ttl_seconds(),
        }
    }
}

fn default_cache_max_entries() -> u64 {
    10_000
}

fn default_cache_ttl_seconds() -> u64 {
    60
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ShardRole {
    /// Accepts writes, forwards to replicas asynchronously.
    #[default]
    Primary,
    /// Read-only; receives writes forwarded from primary.
    Replica,
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
    /// Primary or Replica role for this shard instance.
    #[serde(default)]
    pub role: ShardRole,
    /// gRPC endpoints of replicas (primary only).
    #[serde(default)]
    pub replica_endpoints: Vec<String>,
    /// Template for replica endpoint; `{ordinal}` is replaced with shard_id.
    /// Used in K8s where all primaries share one ConfigMap entry.
    #[serde(default)]
    pub replica_endpoint_template: Option<String>,
    /// gRPC endpoint of the primary this replica should sync from (replica only).
    #[serde(default)]
    pub primary_endpoint: Option<String>,
    /// Template for primary endpoint; `{ordinal}` is replaced with shard_id.
    #[serde(default)]
    pub primary_endpoint_template: Option<String>,
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
            role: ShardRole::Primary,
            replica_endpoints: Vec::new(),
            replica_endpoint_template: None,
            primary_endpoint: None,
            primary_endpoint_template: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Suggest shard config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestShardConfig {
    /// gRPC port for this suggest shard. Default: 9002.
    #[serde(default = "default_suggest_grpc_port")]
    pub grpc_port: u16,
    /// Shard ordinal within the suggest cluster. Default: 0.
    #[serde(default)]
    pub shard_id: u32,
    /// Number of suggest shards in the cluster. Default: 2.
    #[serde(default = "default_num_suggest_shards")]
    pub num_suggest_shards: u32,
    /// Drop terms with doc_freq below this after build. Default: 2.
    #[serde(default = "default_min_doc_frequency")]
    pub min_doc_frequency: u32,
    /// Upper bound on terms kept per shard (top by doc_freq). Default: 50_000.
    #[serde(default = "default_max_terms_per_shard")]
    pub max_terms_per_shard: u32,
    /// Fields tokenized for suggest. Default: ["title"].
    #[serde(default = "default_suggest_fields")]
    pub fields: Vec<String>,
}

impl Default for SuggestShardConfig {
    fn default() -> Self {
        Self {
            grpc_port: default_suggest_grpc_port(),
            shard_id: 0,
            num_suggest_shards: default_num_suggest_shards(),
            min_doc_frequency: default_min_doc_frequency(),
            max_terms_per_shard: default_max_terms_per_shard(),
            fields: default_suggest_fields(),
        }
    }
}

fn default_suggest_grpc_port() -> u16 { 9002 }
fn default_num_suggest_shards() -> u32 { 2 }
fn default_min_doc_frequency() -> u32 { 2 }
fn default_max_terms_per_shard() -> u32 { 50_000 }
fn default_suggest_fields() -> Vec<String> { vec!["title".into()] }

/// Per-shard group config used by the router (primary + optional replicas).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShardGroupConfig {
    pub primary: String,
    #[serde(default)]
    pub replicas: Vec<String>,
}

// ---------------------------------------------------------------------------
// Availability strategy config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AvailabilityConfig {
    #[serde(default)]
    pub strategy: AvailabilityStrategyType,
    #[serde(default)]
    pub r#static: Option<StaticAvailabilityConfig>,
    #[serde(default)]
    pub region_stock: Option<RegionStockConfig>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityStrategyType {
    #[default]
    Noop,
    Static,
    RegionStock,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticAvailabilityConfig {
    /// Attribute key prefix; full key = prefix + zone name. Default: "stock_".
    #[serde(default = "default_stock_prefix")]
    pub stock_attribute_prefix: String,
    /// If the stock attribute is absent, treat the doc as available. Default: true.
    #[serde(default = "default_true_bool")]
    pub default_available: bool,
    /// Minimum numeric stock value to consider the item available. Default: 1.0.
    #[serde(default = "default_min_stock")]
    pub min_stock: f64,
}

impl Default for StaticAvailabilityConfig {
    fn default() -> Self {
        Self {
            stock_attribute_prefix: default_stock_prefix(),
            default_available: true,
            min_stock: default_min_stock(),
        }
    }
}

fn default_stock_prefix() -> String {
    "stock_".into()
}

fn default_true_bool() -> bool {
    true
}

fn default_min_stock() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionStockConfig {
    pub base_url: String,
    #[serde(default = "default_stock_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_stock_timeout_ms() -> u64 {
    50
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
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
}

impl Default for RankerConfig {
    fn default() -> Self {
        Self {
            r#type: default_ranker_type(),
            grpc_endpoint: None,
            wasm_module: None,
            timeout_ms: default_ranker_timeout_ms(),
            candidates: default_ranker_candidates(),
            circuit_breaker: CircuitBreakerConfig::default(),
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
