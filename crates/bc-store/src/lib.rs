//! bc-store: the embedded storage engine that replaces SQL Server.
//!
//! * rows live in memory in ordered B-trees keyed by a memcomparable
//!   encoding of the primary key (case-insensitive like SQL's CI collation)
//! * every secondary key is a B-tree of (key fields + PK) → PK
//! * write transactions are serialized (one writer token) with an undo log,
//!   so there are no deadlocks; readers never block
//! * durability: each commit appends one redo record to a WAL (fdatasync);
//!   startup replays it; `compact()` rewrites it as a snapshot
//!
//! The runtime uses the `Backend`/`Conn` traits; `PgBackend` (PostgreSQL) is
//! the system of record, `MemBackend` wraps the embedded engine for tests.

pub mod backend;
pub mod filter;
pub mod pg;
pub mod store;
pub mod table;
pub mod value;

pub use backend::{Agg, AggOut, Backend, Conn, KeyLock, MemBackend, ScanReq, SeqOp, StoreStats};
pub use pg::PgBackend;
pub use store::{Resolver, Store, Txn};
pub use table::{prefix_end, Key, StoreError, TableData, TableSchema};
pub use value::{cmp_ci, encode_key_part, guid_sort_key, ColType, Row, Value, DATETIME_UNDEFINED};
