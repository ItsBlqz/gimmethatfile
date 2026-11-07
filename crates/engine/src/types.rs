use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type DownloadId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Waiting for a free slot.
    Queued,
    /// Probing the server.
    Connecting,
    Downloading,
    Paused,
    Completed,
    Failed,
}

/// A half-open byte range `[pos, end)` that still has to be downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub pos: u64,
    pub end: u64,
}

/// What is persisted to disk for each download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: DownloadId,
    pub url: String,
    pub dir: PathBuf,
    /// Final file name. `None` until the server has been probed.
    pub file_name: Option<String>,
    pub total: Option<u64>,
    /// Server supports byte ranges, so the download is segmented and resumable.
    pub resumable: bool,
    pub status: Status,
    pub error: Option<String>,
    /// Byte ranges that are still missing (only meaningful when `resumable`).
    #[serde(default)]
    pub segments: Vec<Range>,
    pub downloaded: u64,
    pub connections: u32,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub added_at: u64,
    pub completed_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// How many downloads run at the same time.
    pub max_concurrent: u32,
    /// Connections per download.
    pub connections: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| PathBuf::from(".")),
            max_concurrent: 3,
            connections: 8,
        }
    }
}

/// Live view of a download, sent to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub id: DownloadId,
    pub url: String,
    pub file_name: Option<String>,
    pub dir: String,
    pub total: Option<u64>,
    pub downloaded: u64,
    /// Bytes per second.
    pub speed: f64,
    /// Seconds remaining.
    pub eta: Option<f64>,
    pub status: Status,
    pub error: Option<String>,
    pub resumable: bool,
    /// Currently open connections.
    pub connections: u32,
    pub max_connections: u32,
    /// Ranges still missing; the UI draws these as gaps in the progress bar.
    pub segments: Vec<Range>,
    pub added_at: u64,
    pub completed_at: Option<u64>,
}
