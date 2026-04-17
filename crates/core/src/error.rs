use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Invalid document: {0}")]
    InvalidDocument(String),

    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    #[error("Shard error: {0}")]
    Shard(String),

    #[error("Index error: {0}")]
    Index(String),

    #[error("WAL error: {0}")]
    Wal(String),

    #[error("gRPC error: {0}")]
    Grpc(String),

    #[error("Ranker error: {0}")]
    Ranker(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Document not found: {0}")]
    NotFound(u64),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Internal error: {0}")]
    Internal(String),
}
