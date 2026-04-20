# Requirements: Shard Replication

## Feature Overview

Each shard runs as a **primary + N replicas**. Writes go to the primary synchronously — the client gets an ACK only after the primary has durably indexed the document. The primary then forwards the write to its replicas asynchronously in the background. Replicas never receive writes directly from the router. Reads can be distributed across primary and replicas for throughput.

This model gives:
- **No write latency increase** — replicas don't block the write path
- **Primary-first durability guarantee** — a document is always on the primary before any replica sees it
- **Read scalability** — replicas absorb query load
- **Fault tolerance** — if a shard pod dies, at least one replica still serves reads

## User Stories

### US-1: Write durability without replica blocking
**As a** client indexing a document  
**I want** the index call to return as soon as the primary has committed the write  
**So that** replica lag or failure doesn't increase write latency

**Acceptance Criteria:**
- `POST /v1/index` and `POST /v1/bulk` return as soon as primary ACKs
- Replica forwarding happens in a background task, invisible to the caller
- If a replica is down, its queue accumulates; the write is not lost

### US-2: Replica catch-up on restart
**As an** operator restarting a replica pod  
**I want** the replica to automatically catch up from the primary  
**So that** I don't have to manually trigger a reindex after a replica restart

**Acceptance Criteria:**
- On restart, replica requests all writes it missed since its last WAL offset
- Primary streams missed entries to the replica (WAL-based catch-up)
- Replica is marked as "syncing" until caught up; it serves reads only after fully caught up

### US-3: Read load balancing
**As** the router  
**I want to** fan search queries to any healthy replica (or primary) of each shard  
**So that** read throughput scales with the number of replicas

**Acceptance Criteria:**
- Router maintains a list of healthy endpoints per shard (primary + replicas)
- Each search picks one endpoint per shard using round-robin or random selection
- An unhealthy endpoint is skipped; the shard still participates in search via remaining endpoints
- Primary is always a valid read target (no read-primary-only mode required)

### US-4: Manual promotion
**As an** operator  
**I want to** promote a replica to primary via an admin API call  
**So that** I can recover if the primary pod is permanently lost

**Acceptance Criteria:**
- `POST /v1/admin/shards/{id}/promote?replica=N` promotes replica N to primary
- The promoted node starts accepting writes
- The old primary (if alive) becomes a replica and begins catching up
- No automatic failover — promotion is always an explicit operator action

## Functional Requirements

| ID | Requirement |
|----|------------|
| FR-01 | Each shard has one primary and 0–N replicas (configurable per shard) |
| FR-02 | All writes (index, bulk, delete) go to the primary only |
| FR-03 | Primary ACKs the write before forwarding to replicas |
| FR-04 | Primary forwards writes to replicas asynchronously via a per-replica background queue |
| FR-05 | Replica forwarding uses gRPC `Replicate` RPC (same as existing `Index`/`BulkIndex`/`Delete`) |
| FR-06 | Per-replica queue is bounded (configurable); backpressure drops the oldest entries if full |
| FR-07 | On replica restart, it requests WAL replay from primary (catch-up via `ReplicateCatchUp` RPC) |
| FR-08 | Replica is only added to the read pool after catch-up is complete |
| FR-09 | Router distributes search queries across all healthy endpoints (primary + caught-up replicas) |
| FR-10 | `GET /v1/stats` reports per-shard replica count, sync status, and replication lag (entries behind) |
| FR-11 | `POST /v1/admin/shards/{id}/promote?replica=N` promotes a replica to primary |
| FR-12 | No automatic failover — all topology changes are operator-initiated |

## Non-Functional Requirements

| ID | Requirement | Target |
|----|------------|--------|
| NFR-01 | Write latency overhead from replication | 0ms (async, non-blocking) |
| NFR-02 | Replica lag under normal load | < 1 second |
| NFR-03 | Catch-up throughput | >= 50,000 docs/sec (matches bulk indexing) |
| NFR-04 | Read throughput scaling | Linear with replica count |
| NFR-05 | No data loss on primary crash | WAL on primary ensures durability before any ACK |

## Constraints

- Replication is **asynchronous** — replicas may be behind the primary by seconds under load
- **No automatic failover** — the system does not self-heal; an operator must promote a replica
- Replicas are **read-only** — they reject direct writes with an error
- Replication topology is defined in the shard config (not discovered dynamically)

## Out of Scope

- Automatic leader election (Raft, Paxos, etc.)
- Multi-primary / active-active writes
- Cross-datacenter replication
- Replica-local write buffering (replicas apply writes immediately, no write buffer of their own)
- Replication of delete tombstones across shard boundaries
