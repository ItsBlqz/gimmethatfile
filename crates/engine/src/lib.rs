//! Gimme That File engine: multi-connection, resumable HTTP downloads.

mod fsutil;
mod job;
mod manager;
pub mod naming;
mod probe;
mod types;

pub use manager::Manager;
pub use types::{DownloadId, Range, Settings, Snapshot, Status};
