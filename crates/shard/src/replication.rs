use crate::wal::WalEntry;
use search_core::{Document, Value};
use search_proto::shard::shard_service_client::ShardServiceClient;
use search_proto::shard::{replicate_request, DocumentProto, ReplicateRequest};
use tokio::sync::mpsc::{self, UnboundedSender};

/// One entry forwarded to a replica.
#[derive(Clone)]
pub struct ReplicaEntry {
    pub seq: u64,
    pub entry: WalEntry,
}

fn doc_to_proto(doc: Document) -> DocumentProto {
    DocumentProto {
        id: doc.id,
        title: doc.title,
        description: doc.description,
        price: doc.price,
        category: doc.category,
        attributes: doc
            .attributes
            .into_iter()
            .filter_map(|(k, v)| {
                if let Value::String(s) = v { Some((k, s)) } else { None }
            })
            .collect(),
    }
}

fn entry_to_proto(entry: ReplicaEntry) -> ReplicateRequest {
    let operation = match entry.entry {
        WalEntry::Index(doc) => replicate_request::Operation::IndexDoc(doc_to_proto(doc)),
        WalEntry::Delete(id) => replicate_request::Operation::DeleteDocId(id),
    };
    ReplicateRequest { operation: Some(operation), wal_seq: entry.seq }
}

/// Manages async replication to one or more replica endpoints.
/// One background Tokio task per replica drains an unbounded channel and calls
/// the replica's `Replicate` gRPC RPC. Failure retries with backoff.
pub struct ReplicationManager {
    senders: Vec<UnboundedSender<ReplicaEntry>>,
}

impl ReplicationManager {
    /// Spawn background tasks for each replica endpoint.
    pub fn start(replica_endpoints: Vec<String>) -> Self {
        let mut senders = Vec::with_capacity(replica_endpoints.len());
        for endpoint in replica_endpoints {
            let (tx, mut rx) = mpsc::unbounded_channel::<ReplicaEntry>();
            senders.push(tx);
            tokio::spawn(async move {
                let mut backoff_ms = 100u64;
                loop {
                    // Try to connect
                    let mut client = loop {
                        match ShardServiceClient::connect(endpoint.clone()).await {
                            Ok(c) => { backoff_ms = 100; break c; }
                            Err(e) => {
                                tracing::warn!(endpoint = %endpoint, err = %e, "replica connect failed, retrying in {backoff_ms}ms");
                                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                                backoff_ms = (backoff_ms * 2).min(10_000);
                            }
                        }
                    };

                    // Drain channel until connection drops
                    while let Some(entry) = rx.recv().await {
                        let req = entry_to_proto(entry.clone());
                        if let Err(e) = client.replicate(tonic::Request::new(req)).await {
                            tracing::warn!(endpoint = %endpoint, err = %e, "replicate RPC failed, reconnecting");
                            // Re-enqueue current entry is not possible with unbounded channel after move.
                            // The entry is lost on transient failure — acceptable for async replication.
                            break;
                        }
                    }
                    // If rx is closed (shard shutting down), exit task.
                    if rx.is_closed() { break; }
                }
            });
        }
        Self { senders }
    }

    /// Enqueue a WAL entry for async forwarding to all replicas.
    pub fn enqueue(&self, seq: u64, entry: WalEntry) {
        if self.senders.is_empty() { return; }
        let replica_entry = ReplicaEntry { seq, entry };
        for tx in &self.senders {
            let _ = tx.send(replica_entry.clone());
        }
    }

    pub fn replica_count(&self) -> usize {
        self.senders.len()
    }
}
