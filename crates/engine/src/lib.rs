//! Gimme That File engine: multi-connection, resumable HTTP downloads.

mod control;
mod fsutil;
mod headers;
mod job;
mod limit;
mod manager;
pub mod naming;
mod probe;
mod schedule;
mod types;

pub use manager::Manager;
pub use schedule::Schedule;
pub use types::{AddOptions, Checksum, DownloadId, Event, Range, Settings, Snapshot, Status};
