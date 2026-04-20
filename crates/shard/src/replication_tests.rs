//! Integration tests for shard replication — require a live gRPC server in-process.
//! These tests spin up real tonic servers on random ports, so they test the full
//! replication path: primary WAL → ReplicationManager → gRPC → replica apply.

#[cfg(test)]
mod tests {
    use crate::grpc_server::ShardGrpcServer;
    use crate::shard::ShardEngine;
    use search_core::{Document, IndexSchema, SearchRequest, ShardConfig, ShardRole, Value};
    use search_proto::shard::shard_service_server::ShardServiceServer;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;

    fn make_config(data_dir: &std::path::Path) -> ShardConfig {
        ShardConfig {
            data_dir: data_dir.to_path_buf(),
            write_buffer_size: 1024 * 1024,
            ..Default::default()
        }
    }

    fn make_doc(id: u64, title: &str, category: &str) -> Document {
        Document {
            id,
            title: title.into(),
            description: "great product".into(),
            price: id as f64 * 100.0,
            category: category.into(),
            attributes: HashMap::new(),
        }
    }

    /// Bind a random port, start a tonic server for the given shard, return the address.
    async fn start_grpc(shard: Arc<ShardEngine>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(ShardServiceServer::new(ShardGrpcServer::new(shard)))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        });
        // Give the server a moment to bind before clients connect.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        addr
    }

    // -----------------------------------------------------------------------
    // T-11a: Replicate RPC rejected on primary
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_replicate_rpc_rejected_on_primary() {
        use search_proto::shard::shard_service_client::ShardServiceClient;
        use search_proto::shard::{replicate_request, ReplicateRequest};

        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let primary = Arc::new(ShardEngine::open(make_config(dir.path()), schema).await.unwrap());
        let addr = start_grpc(primary).await;

        let mut client = ShardServiceClient::connect(format!("http://{addr}")).await.unwrap();
        let result = client.replicate(tonic::Request::new(ReplicateRequest {
            operation: Some(replicate_request::Operation::DeleteDocId(99)),
            wal_seq: 0,
        })).await;

        assert!(result.is_err(), "Replicate RPC must be rejected on a primary");
        assert_eq!(result.unwrap_err().code(), tonic::Code::FailedPrecondition);
    }

    // -----------------------------------------------------------------------
    // T-11b: Replica catch-up from primary on startup
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_replica_catches_up_on_startup() {
        let dir_p = tempfile::TempDir::new().unwrap();
        let dir_r = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        // Start primary gRPC server (no replica configured yet)
        let primary = Arc::new(ShardEngine::open(make_config(dir_p.path()), schema.clone()).await.unwrap());
        let primary_addr = start_grpc(Arc::clone(&primary)).await;

        // Index 5 docs on primary before replica exists
        for i in 1..=5u64 {
            primary.index(make_doc(i, "Samsung Galaxy product", "electronics")).unwrap();
        }

        // Start replica pointing at primary — catch-up happens inside open()
        let mut replica_cfg = make_config(dir_r.path());
        replica_cfg.role = ShardRole::Replica;
        replica_cfg.primary_endpoint = Some(format!("http://{primary_addr}"));
        let replica = ShardEngine::open(replica_cfg, schema).await.unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = replica.search(req).unwrap();
        assert_eq!(resp.total_hits, 5, "replica should have caught up all 5 docs from primary");
    }

    // -----------------------------------------------------------------------
    // T-11c: Live replication — index on primary, doc appears on replica
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_primary_replicates_to_replica() {
        let dir_p = tempfile::TempDir::new().unwrap();
        let dir_r = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        // Start replica gRPC server first so primary can connect immediately
        let mut replica_cfg = make_config(dir_r.path());
        replica_cfg.role = ShardRole::Replica;
        let replica = Arc::new(ShardEngine::open(replica_cfg, schema.clone()).await.unwrap());
        let replica_addr = start_grpc(Arc::clone(&replica)).await;

        // Start primary with replica endpoint
        let mut primary_cfg = make_config(dir_p.path());
        primary_cfg.replica_endpoints = vec![format!("http://{replica_addr}")];
        let primary = ShardEngine::open(primary_cfg, schema).await.unwrap();

        // Index two docs on primary
        primary.index(make_doc(1, "Samsung Galaxy phone", "electronics")).unwrap();
        primary.index(make_doc(2, "Apple iPhone device", "electronics")).unwrap();

        // Wait for async replication to deliver
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = replica.search(req).unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 1);
    }

    // -----------------------------------------------------------------------
    // T-11d: Delete on primary propagates to replica
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_delete_replicates_to_replica() {
        let dir_p = tempfile::TempDir::new().unwrap();
        let dir_r = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        let mut replica_cfg = make_config(dir_r.path());
        replica_cfg.role = ShardRole::Replica;
        let replica = Arc::new(ShardEngine::open(replica_cfg, schema.clone()).await.unwrap());
        let replica_addr = start_grpc(Arc::clone(&replica)).await;

        let mut primary_cfg = make_config(dir_p.path());
        primary_cfg.replica_endpoints = vec![format!("http://{replica_addr}")];
        let primary = ShardEngine::open(primary_cfg, schema).await.unwrap();

        primary.index(make_doc(1, "Samsung Galaxy", "electronics")).unwrap();
        primary.index(make_doc(2, "Apple iPhone", "electronics")).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Both docs on replica before delete
        let req = SearchRequest { query: "samsung".into(), limit: 5, ..Default::default() };
        assert_eq!(replica.search(req.clone()).unwrap().total_hits, 1);

        primary.delete(1).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Doc 1 deleted on replica
        assert_eq!(replica.search(req).unwrap().total_hits, 0, "delete must propagate to replica");
    }
}
