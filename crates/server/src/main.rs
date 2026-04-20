use axum::{
    body::Bytes,
    extract::{Path, Query, Request, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use clap::Parser;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use search_core::{config::Mode, Document, SearchRequest, ShardConfig, TelemetryConfig};
use search_meli::{MeliItem, MeliMapper};
use search_ranker::{NoopRanker, RankerFactory};
use search_router::{
    build_strategy, BreakerShardClient, LocalShardClient, LocalSuggestClient, RemoteShardClient,
    ShardClient,
};
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
    for group in stats.shards.iter() {
        let id = group.primary.shard_id.to_string();
        metrics::gauge!("shard_doc_count", "shard_id" => id.clone())
            .set(group.primary.doc_count as f64);
        metrics::gauge!("shard_segment_count", "shard_id" => id)
            .set(group.primary.segment_count as f64);
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

#[derive(serde::Deserialize)]
struct SuggestParams {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    field: Option<String>,
}

#[derive(serde::Serialize)]
struct SuggestResponse {
    suggestions: Vec<search_suggest::SuggestTerm>,
    consistency: &'static str,
    took_ms: u128,
}

async fn suggest(
    State(state): State<AppState>,
    Query(params): Query<SuggestParams>,
) -> Result<Json<SuggestResponse>, (StatusCode, Json<Value>)> {
    let q = params.q.trim();
    if q.is_empty() {
        return Ok(Json(SuggestResponse {
            suggestions: Vec::new(),
            consistency: "eventual",
            took_ms: 0,
        }));
    }
    if q.len() > 100 {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "q too long (max 100)" }))));
    }
    let limit = params.limit.unwrap_or(5).clamp(1, 20);
    let field = params.field.unwrap_or_else(|| "title".into());

    let start = std::time::Instant::now();
    let result = state.router.suggest(q, &field, limit).await;
    let elapsed = start.elapsed();
    metrics::histogram!("suggest_duration_seconds").record(elapsed.as_secs_f64());

    match result {
        Ok(entries) => Ok(Json(SuggestResponse {
            suggestions: entries,
            consistency: "eventual",
            took_ms: elapsed.as_millis(),
        })),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })))),
    }
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

async fn meli_bulk(
    State(state): State<AppState>,
    request: Request,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let body = axum::body::to_bytes(request.into_body(), 256 * 1024 * 1024)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))))?;

    let items: Vec<MeliItem> = if content_type.contains("ndjson")
        || content_type.contains("jsonlines")
        || content_type.contains("jsonl")
    {
        parse_jsonl(&body)
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))))?
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))))?
    };

    if items.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "empty batch" }))));
    }
    if items.len() > 10_000 {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "batch exceeds 10,000 items" }))));
    }

    let (docs, errors) = MeliMapper::map_batch(items);
    let indexed = docs.len();
    let skipped: Vec<String> = errors.iter().map(|e| e.to_string()).collect();

    if indexed == 0 {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "all items failed mapping", "details": skipped }))));
    }

    state
        .router
        .bulk_index(docs)
        .await
        .map(|_| Json(json!({ "indexed": indexed, "skipped": skipped.len(), "errors": skipped })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))
}

fn parse_jsonl(body: &Bytes) -> Result<Vec<MeliItem>, String> {
    let text = std::str::from_utf8(body).map_err(|e| e.to_string())?;
    let mut items = Vec::new();
    for (line_num, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let item: MeliItem = serde_json::from_str(line)
            .map_err(|e| format!("line {}: {}", line_num + 1, e))?;
        items.push(item);
    }
    Ok(items)
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

async fn reindex_all(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let t0 = std::time::Instant::now();
    state
        .router
        .reindex()
        .await
        .map(|shard_stats| {
            let total_docs: u64 = shard_stats.iter().map(|s| s.docs_reindexed).sum();
            let shards: Vec<Value> = shard_stats.iter().map(|s| json!({
                "shard_id": s.shard_id,
                "docs_reindexed": s.docs_reindexed,
                "elapsed_ms": s.elapsed_ms,
            })).collect();
            Json(json!({
                "ok": true,
                "total_docs": total_docs,
                "total_elapsed_ms": t0.elapsed().as_millis() as u64,
                "shards": shards,
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))
}

async fn admin_ui() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("admin.html"))
}

async fn stats(State(state): State<AppState>) -> Json<Value> {
    let router_stats = state.router.stats().await;
    let total_docs: u64 = router_stats.shards.iter().map(|s| s.primary.doc_count).sum();
    let total_segments: u32 = router_stats.shards.iter().map(|s| s.primary.segment_count).sum();
    let shards: Vec<Value> = router_stats.shards.iter().map(|s| {
        let replicas: Vec<Value> = s.replicas.iter().map(|r| json!({
            "shard_id": r.shard_id,
            "doc_count": r.doc_count,
            "segment_count": r.segment_count,
            "buffer_doc_count": r.buffer_doc_count,
        })).collect();
        json!({
            "shard_id": s.primary.shard_id,
            "doc_count": s.primary.doc_count,
            "segment_count": s.primary.segment_count,
            "buffer_doc_count": s.primary.buffer_doc_count,
            "replicas": replicas,
        })
    }).collect();
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
        .route("/v1/suggest", get(suggest))
        .route("/v1/index", post(index_doc))
        .route("/v1/bulk", post(bulk_index))
        .route("/v1/ingest/bulk", post(meli_bulk))
        .route("/v1/index/{id}", delete(delete_doc))
        .route("/v1/stats", get(stats))
        .route("/v1/admin/flush", post(flush_all))
        .route("/v1/admin/reindex", post(reindex_all))
        .route("/metrics", get(prometheus_metrics))
        .with_state(state)
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<_>| {
                    tracing::info_span!(
                        "http.request",
                        method = %req.method(),
                        path = %req.uri().path(),
                        status = tracing::field::Empty,
                    )
                })
                .on_response(
                    |res: &axum::http::Response<_>,
                     latency: std::time::Duration,
                     span: &tracing::Span| {
                        span.record("status", res.status().as_u16());
                        tracing::debug!(latency_ms = latency.as_millis(), "response sent");
                    },
                ),
        )
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

async fn start_suggest_grpc_server(engine: Arc<search_suggest::SuggestShardEngine>, port: u16) {
    use search_proto::shard::shard_service_server::ShardServiceServer;
    use search_suggest::SuggestGrpcServer;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("gRPC suggest server listening on {addr}");
    tonic::transport::Server::builder()
        .add_service(ShardServiceServer::new(SuggestGrpcServer::new(engine)))
        .serve_with_shutdown(addr, shutdown_signal())
        .await
        .expect("suggest gRPC server error");
    info!("suggest gRPC server stopped");
}

// ---------------------------------------------------------------------------
// Tracing / telemetry
// ---------------------------------------------------------------------------

fn init_tracing(config: &TelemetryConfig) {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let env_filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let fmt = || tracing_subscriber::fmt::layer().json();

    // Build OTel layer inline so Rust can infer the full Layered<S> subscriber type.
    if config.enabled {
        if let Some(tracer) = build_otel_tracer(config) {
            tracing_subscriber::registry()
                .with(env_filter())
                .with(fmt())
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .init();
            return;
        }
    }
    tracing_subscriber::registry().with(env_filter()).with(fmt()).init();
}

fn build_otel_tracer(config: &TelemetryConfig) -> Option<opentelemetry_sdk::trace::Tracer> {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::trace as sdktrace;

    let sampler = if config.sample_rate >= 1.0 {
        sdktrace::Sampler::AlwaysOn
    } else {
        sdktrace::Sampler::TraceIdRatioBased(config.sample_rate)
    };

    let provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&config.otlp_endpoint),
        )
        .with_trace_config(
            sdktrace::config()
                .with_sampler(sampler)
                .with_resource(opentelemetry_sdk::Resource::new(vec![KeyValue::new(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                    config.service_name.clone(),
                )])),
        )
        .install_batch(opentelemetry_sdk::runtime::Tokio)
        .map_err(|e| eprintln!("Failed to init OTel tracer: {e}"))
        .ok()?;

    let tracer = provider.tracer(config.service_name.clone());
    opentelemetry::global::set_tracer_provider(provider);
    Some(tracer)
}

fn shutdown_telemetry() {
    opentelemetry::global::shutdown_tracer_provider();
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {

    let cli = Cli::parse();

    let mut config = if let Some(ref path) = cli.config {
        search_core::Config::from_file(path).expect("Failed to load config")
    } else {
        search_core::Config::default()
    };

    init_tracing(&config.telemetry);

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

    // Resolve endpoint templates using shard_id ordinal.
    if config.mode == Mode::Shard {
        let ordinal = config.shard.shard_id.to_string();
        if config.shard.replica_endpoints.is_empty() {
            if let Some(tmpl) = config.shard.replica_endpoint_template.take() {
                config.shard.replica_endpoints = vec![tmpl.replace("{ordinal}", &ordinal)];
            }
        }
        if config.shard.primary_endpoint.is_none() {
            if let Some(tmpl) = config.shard.primary_endpoint_template.take() {
                config.shard.primary_endpoint = Some(tmpl.replace("{ordinal}", &ordinal));
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
                ShardEngine::open(config.shard.clone(), schema).await.expect("Failed to open shard"),
            );
            start_grpc_server(Arc::clone(&shard), config.shard.grpc_port).await;
            info!("Flushing shard write buffer on shutdown...");
            if let Err(e) = shard.flush() {
                tracing::error!("Shard flush failed: {e}");
            }
            shutdown_telemetry();
        }

        Mode::Router => {
            info!(port = config.router.http_port, "Starting router");

            let cb_config = &config.router.circuit_breaker;
            let wrap = |client: Arc<dyn ShardClient>| -> Arc<dyn ShardClient> {
                if cb_config.enabled {
                    Arc::new(BreakerShardClient::new(client, cb_config))
                } else {
                    client
                }
            };

            let suggest_clients: Vec<Arc<dyn ShardClient>> = config
                .router
                .suggest_shards
                .iter()
                .map(|ep| Arc::new(RemoteShardClient::new(ep)) as Arc<dyn ShardClient>)
                .collect();

            let router = if !config.router.shards.is_empty() {
                // New format: ShardGroups with primary + replicas
                let groups: Vec<search_router::client::ShardGroup> = config
                    .router
                    .shards
                    .iter()
                    .map(|sg| {
                        let primary = wrap(Arc::new(RemoteShardClient::new(&sg.primary)));
                        let replicas: Vec<Arc<dyn ShardClient>> = sg
                            .replicas
                            .iter()
                            .map(|ep| wrap(Arc::new(RemoteShardClient::new(ep))))
                            .collect();
                        search_router::client::ShardGroup::new(primary, replicas)
                    })
                    .collect();
                if groups.is_empty() {
                    panic!("Router mode requires shards or shard_endpoints in config");
                }
                let ranker = RankerFactory::build(&config.ranker)
                    .unwrap_or_else(|_| Arc::new(NoopRanker));
                let availability = build_strategy(&config.router.availability);
                Arc::new(
                    search_router::Router::with_cache(
                        groups,
                        ranker,
                        config.ranker.timeout_ms,
                        config.ranker.candidates,
                        config.router.query_timeout_ms,
                        availability,
                        &config.router.cache,
                    )
                    .with_suggest_shards(
                        suggest_clients.clone(),
                        config.router.suggest_write_timeout_ms,
                        config.router.suggest_query_timeout_ms,
                    ),
                )
            } else {
                // Legacy format: plain shard_endpoints (no replicas)
                let shards: Vec<Arc<dyn ShardClient>> = config
                    .router
                    .shard_endpoints
                    .iter()
                    .map(|ep| wrap(Arc::new(RemoteShardClient::new(ep))))
                    .collect();
                if shards.is_empty() {
                    panic!("Router mode requires shards or shard_endpoints in config");
                }
                let ranker = RankerFactory::build(&config.ranker)
                    .unwrap_or_else(|_| Arc::new(NoopRanker));
                Arc::new(
                    search_router::Router::new_simple(
                        shards,
                        ranker,
                        config.ranker.timeout_ms,
                        config.ranker.candidates,
                        config.router.query_timeout_ms,
                    )
                    .with_suggest_shards(
                        suggest_clients.clone(),
                        config.router.suggest_write_timeout_ms,
                        config.router.suggest_query_timeout_ms,
                    ),
                )
            };
            start_http_server(Arc::clone(&router), config.router.http_port, metrics_handle).await;
            info!("Flushing shards on shutdown...");
            if let Err(e) = router.flush().await {
                tracing::error!("Router flush failed: {e}");
            }
            shutdown_telemetry();
        }

        Mode::Standalone => {
            info!(
                num_shards = config.shard.num_shards,
                port = config.router.http_port,
                "Starting standalone"
            );
            let num_shards = config.shard.num_shards;
            let base_data_dir = config.shard.data_dir.clone();

            let mut shards: Vec<Arc<dyn ShardClient>> = Vec::new();
            for i in 0..num_shards {
                let shard_config = ShardConfig {
                    shard_id: i,
                    data_dir: base_data_dir.join(format!("shard-{i}")),
                    num_shards,
                    ..config.shard.clone()
                };
                let shard = Arc::new(
                    ShardEngine::open(shard_config, schema.clone())
                        .await
                        .unwrap_or_else(|e| panic!("Failed to open shard {i}: {e}")),
                );
                shards.push(Arc::new(LocalShardClient::new(shard)) as Arc<dyn ShardClient>);
            }

            let ranker = RankerFactory::build(&config.ranker)
                .unwrap_or_else(|_| Arc::new(NoopRanker));

            // Optional in-process suggest cluster. Runs only if num_suggest_shards > 0.
            let suggest_clients: Vec<Arc<dyn ShardClient>> = if config
                .suggest_shard
                .num_suggest_shards
                > 0
            {
                let mut v: Vec<Arc<dyn ShardClient>> = Vec::new();
                let mut engines: Vec<Arc<search_suggest::SuggestShardEngine>> = Vec::new();
                for i in 0..config.suggest_shard.num_suggest_shards {
                    let mut sc = config.suggest_shard.clone();
                    sc.shard_id = i;
                    let engine = Arc::new(search_suggest::SuggestShardEngine::new(sc));
                    engines.push(Arc::clone(&engine));
                    v.push(Arc::new(LocalSuggestClient::new(engine)) as Arc<dyn ShardClient>);
                }
                // Background flush loop for standalone suggest engines.
                tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
                    ticker.tick().await;
                    loop {
                        ticker.tick().await;
                        for engine in &engines {
                            let len = engine.flush();
                            metrics::gauge!(
                                "suggest_index_terms",
                                "shard_id" => engine.config().shard_id.to_string()
                            )
                            .set(len as f64);
                        }
                    }
                });
                v
            } else {
                Vec::new()
            };

            let router = Arc::new(
                search_router::Router::new_simple(
                    shards,
                    ranker,
                    config.ranker.timeout_ms,
                    config.ranker.candidates,
                    config.router.query_timeout_ms,
                )
                .with_suggest_shards(
                    suggest_clients,
                    config.router.suggest_write_timeout_ms,
                    config.router.suggest_query_timeout_ms,
                ),
            );
            start_http_server(Arc::clone(&router), config.router.http_port, metrics_handle).await;
            info!("Flushing shards on shutdown...");
            if let Err(e) = router.flush().await {
                tracing::error!("Router flush failed: {e}");
            }
            shutdown_telemetry();
        }

        Mode::Suggest => {
            info!(
                shard_id = config.suggest_shard.shard_id,
                port = config.suggest_shard.grpc_port,
                "Starting suggest shard"
            );
            let engine = Arc::new(search_suggest::SuggestShardEngine::new(
                config.suggest_shard.clone(),
            ));
            // Periodic background flush to promote the write buffer into the query index.
            {
                let engine = Arc::clone(&engine);
                tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
                    ticker.tick().await;
                    loop {
                        ticker.tick().await;
                        let start = std::time::Instant::now();
                        let len = engine.flush();
                        metrics::histogram!("suggest_index_build_seconds")
                            .record(start.elapsed().as_secs_f64());
                        metrics::gauge!(
                            "suggest_index_terms",
                            "shard_id" => engine.config().shard_id.to_string()
                        )
                        .set(len as f64);
                    }
                });
            }
            start_suggest_grpc_server(Arc::clone(&engine), config.suggest_shard.grpc_port).await;
            shutdown_telemetry();
        }
    }
}
