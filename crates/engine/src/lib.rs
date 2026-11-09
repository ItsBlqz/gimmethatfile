//! Gimme That File engine: multi-connection, resumable HTTP downloads.

mod control;
mod fsutil;
mod job;
mod limit;
mod manager;
pub mod naming;
mod probe;
mod types;

pub use manager::Manager;
pub use types::{AddOptions, DownloadId, Range, Settings, Snapshot, Status};
