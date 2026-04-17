use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use clap::Parser;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use search_core::{config::Mode, Document, SearchRequest, ShardConfig};
use search_ranker::{NoopRanker, RankerFactory};
use search_router::{LocalShardClient, RemoteShardClient, ShardClient};
use search_shard::ShardEngine;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "search-engine", about = "Distributed product search engine")]
struct Cli {
    #[arg(long, value_enum, default_value = "standalone")]
    mode: Mode,
    #[arg(long, short)]
    config: Option<PathBuf>,
    #[arg(long, default_value = "4")]
    num_shards: u32,
    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,
    #[arg(long, default_value = "8080")]
    http_port: u16,
    #[arg(long, default_value = "9001")]
    grpc_port: u16,
    /// Shard ID (shard mode only). Auto-detected from HOSTNAME in K8s StatefulSets.
    #[arg(long, default_value = "0")]
    shard_id: u32,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    router: Arc<search_router::Router>,
    metrics: PrometheusHandle,
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn prometheus_metrics(State(state): State<AppState>) -> String {
    // Update per-shard gauges before rendering
    let stats = state.router.stats().await;
    for (i, shard) in stats.shards.iter().enumerate() {
        let id = shard.shard_id.to_string();
        metrics::gauge!("shard_doc_count", "shard_id" => id.clone())
            .set(shard.doc_count as f64);
        metrics::gauge!("shard_segment_count", "shard_id" => id)
            .set(shard.segment_count as f64);
        let _ = i;
    }
    state.metrics.render()
}

async fn search(
    State(state): State<AppState>,
    Json(request): Json<SearchRequest>,
) -> Result<Json<search_core::SearchResponse>, (StatusCode, Json<Value>)> {
    if request.query.len() > 1024 {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "query too long" }))));
    }
    if request.limit > 1000 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "limit exceeds maximum of 1000" })),
        ));
    }

    state
        .router
        .search(request)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))
}

async fn index_doc(
    State(state): State<AppState>,
    Json(doc): Json<Document>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if doc.title.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "title is required" }))));
    }

    state
        .router
        .index(doc)
        .await
        .map(|_| Json(json!({ "success": true })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))
}

async fn bulk_index(
    State(state): State<AppState>,
    Json(docs): Json<Vec<Document>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if docs.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "empty batch" }))));
    }
    if docs.len() > 10_000 {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "batch exceeds 10,000 documents" }))));
    }
    if docs.iter().any(|d| d.title.is_empty()) {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "all documents must have a title" }))));
    }
    state
        .router
        .bulk_index(docs)
        .await
        .map(|indexed| Json(json!({ "indexed": indexed })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))
}

async fn delete_doc(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state
        .router
        .delete(id)
        .await
        .map(|_| Json(json!({ "success": true })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))
}

async fn flush_all(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state
        .router
        .flush()
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))
}

async fn admin_ui() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("admin.html"))
}

async fn stats(State(state): State<AppState>) -> Json<Value> {
    let router_stats = state.router.stats().await;
    let total_docs: u64 = router_stats.shards.iter().map(|s| s.doc_count).sum();
    let total_segments: u32 = router_stats.shards.iter().map(|s| s.segment_count).sum();
    let shards: Vec<Value> = router_stats.shards.iter().map(|s| json!({
        "shard_id": s.shard_id,
        "doc_count": s.doc_count,
        "segment_count": s.segment_count,
        "buffer_doc_count": s.buffer_doc_count,
    })).collect();
    Json(json!({
        "total_docs": total_docs,
        "total_segments": total_segments,
        "shards": shards,
    }))
}

// ---------------------------------------------------------------------------
// Server startup helpers
// ---------------------------------------------------------------------------

fn build_http_app(router: Arc<search_router::Router>, metrics: PrometheusHandle) -> axum::Router {
    let state = AppState { router, metrics };
    Router::new()
        .route("/admin", get(admin_ui))
        .route("/v1/health", get(health))
        .route("/v1/search", post(search))
        .route("/v1/index", post(index_doc))
        .route("/v1/bulk", post(bulk_index))
        .route("/v1/index/{id}", delete(delete_doc))
        .route("/v1/stats", get(stats))
        .route("/v1/admin/flush", post(flush_all))
        .route("/metrics", get(prometheus_metrics))
        .with_state(state)
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    info!("Shutdown signal received, draining connections...");
}

async fn start_http_server(router: Arc<search_router::Router>, port: u16, metrics: PrometheusHandle) {
    let app = build_http_app(router, metrics);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("HTTP server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind HTTP port");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("HTTP server error");
    info!("HTTP server stopped");
}

async fn start_grpc_server(shard: Arc<ShardEngine>, port: u16) {
    use search_proto::shard::shard_service_server::ShardServiceServer;
    use search_shard::ShardGrpcServer;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("gRPC shard server listening on {addr}");
    tonic::transport::Server::builder()
        .add_service(ShardServiceServer::new(ShardGrpcServer::new(shard)))
        .serve_with_shutdown(addr, shutdown_signal())
        .await
        .expect("gRPC server error");
    info!("gRPC server stopped");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    let cli = Cli::parse();

    let mut config = if let Some(ref path) = cli.config {
        search_core::Config::from_file(path).expect("Failed to load config")
    } else {
        search_core::Config::default()
    };

    config.mode = cli.mode;
    config.shard.num_shards = cli.num_shards;
    config.shard.data_dir = cli.data_dir;
    config.router.http_port = cli.http_port;
    config.shard.grpc_port = cli.grpc_port;
    config.shard.shard_id = cli.shard_id;

    // In K8s StatefulSets, HOSTNAME is "pod-name-<ordinal>" — auto-detect shard_id.
    if config.mode == Mode::Shard && config.shard.shard_id == 0 {
        if let Ok(hostname) = std::env::var("HOSTNAME") {
            if let Some(ordinal) = hostname
                .rsplit('-')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
            {
                config.shard.shard_id = ordinal;
                info!(shard_id = ordinal, hostname, "Auto-detected shard_id from HOSTNAME");
            }
        }
    }

    // Install Prometheus metrics recorder
    let metrics_handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("Failed to install Prometheus recorder");

    info!(mode = ?config.mode, "Starting search engine");

    let schema = search_core::IndexSchema::default_product_schema();

    match config.mode {
        Mode::Shard => {
            info!(
                shard_id = config.shard.shard_id,
                port = config.shard.grpc_port,
                "Starting shard"
            );
            let shard = Arc::new(
                ShardEngine::open(config.shard.clone(), schema).expect("Failed to open shard"),
            );
            start_grpc_server(shard, config.shard.grpc_port).await;
        }

        Mode::Router => {
            info!(
                port = config.router.http_port,
                shards = ?config.router.shard_endpoints,
                "Starting router"
            );
            let shards: Vec<Arc<dyn ShardClient>> = config
                .router
                .shard_endpoints
                .iter()
                .map(|ep| Arc::new(RemoteShardClient::new(ep)) as Arc<dyn ShardClient>)
                .collect();

            if shards.is_empty() {
                panic!("Router mode requires shard_endpoints in config");
            }

            let ranker = RankerFactory::build(&config.ranker)
                .unwrap_or_else(|_| Arc::new(NoopRanker));
            let router = Arc::new(search_router::Router::new(
                shards,
                ranker,
                config.ranker.timeout_ms,
                config.ranker.candidates,
                config.router.query_timeout_ms,
            ));
            start_http_server(router, config.router.http_port, metrics_handle).await;
        }

        Mode::Standalone => {
            info!(
                num_shards = config.shard.num_shards,
                port = config.router.http_port,
                "Starting standalone"
            );
            let num_shards = config.shard.num_shards;
            let base_data_dir = config.shard.data_dir.clone();

            let shards: Vec<Arc<dyn ShardClient>> = (0..num_shards)
                .map(|i| {
                    let shard_config = ShardConfig {
                        shard_id: i,
                        data_dir: base_data_dir.join(format!("shard-{i}")),
                        num_shards,
                        ..config.shard.clone()
                    };
                    let shard = Arc::new(
                        ShardEngine::open(shard_config, schema.clone())
                            .unwrap_or_else(|e| panic!("Failed to open shard {i}: {e}")),
                    );
                    Arc::new(LocalShardClient::new(shard)) as Arc<dyn ShardClient>
                })
                .collect();

            let ranker = RankerFactory::build(&config.ranker)
                .unwrap_or_else(|_| Arc::new(NoopRanker));
            let router = Arc::new(search_router::Router::new(
                shards,
                ranker,
                config.ranker.timeout_ms,
                config.ranker.candidates,
                config.router.query_timeout_ms,
            ));
            start_http_server(router, config.router.http_port, metrics_handle).await;
        }
    }
}
