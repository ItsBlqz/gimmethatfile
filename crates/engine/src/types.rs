use crate::schedule::Schedule;
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
    /// File name suggested by whoever added the download (e.g. the browser).
    /// Used instead of the server's name when the file is first probed.
    #[serde(default)]
    pub name_hint: Option<String>,
    /// Extra request headers (Cookie, Referer, User-Agent, ...) sent with
    /// every request for this download, e.g. captured from the browser.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Per-download speed limit in bytes/second. `None` = unlimited.
    #[serde(default)]
    pub speed_limit: Option<u64>,
    /// Expected SHA-256 (hex) to verify after completion.
    #[serde(default)]
    pub expected_sha256: Option<String>,
    /// Result of checking `expected_sha256`. `None` when there is nothing to
    /// check (yet).
    #[serde(default)]
    pub checksum: Option<Checksum>,
}

/// SHA-256 verification state of a completed download.
///
/// A mismatch leaves the download `Completed` (the file is kept as the server
/// sent it); the UI shows a warning so the user can decide what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Checksum {
    /// The file is being hashed.
    Verifying,
    /// The file matches the expected hash.
    Ok,
    /// The file does not match (or could not be read).
    Mismatch,
}

/// Something that happened to a download, for notifications.
/// Only sent for downloads that finish while the app runs, never for pauses
/// or removals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Downloaded (and, if a hash was given, verified).
    Completed { id: DownloadId, file_name: String },
    /// Downloaded, but the SHA-256 differs from the expected one.
    ChecksumMismatch { id: DownloadId, file_name: String },
    Failed {
        id: DownloadId,
        file_name: String,
        error: String,
    },
}

/// Options for [`crate::Manager::add_with`]. Everything is optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AddOptions {
    /// Folder to save into; defaults to the settings' download folder.
    pub dir: Option<PathBuf>,
    /// Connections for this download; defaults to the settings.
    pub connections: Option<u32>,
    /// Preferred file name (sanitized; made unique on probe).
    pub name_hint: Option<String>,
    /// Extra request headers.
    pub headers: Vec<(String, String)>,
    /// Bytes/second; `None` = unlimited.
    pub speed_limit: Option<u64>,
    /// Expected SHA-256 (hex).
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// How many downloads run at the same time.
    pub max_concurrent: u32,
    /// Connections per download.
    pub connections: u32,
    /// Global speed limit for all downloads together, bytes/second.
    /// `None` = unlimited.
    pub speed_limit: Option<u64>,
    /// Minimize button hides to the tray (the close button always does).
    pub minimize_to_tray: bool,
    /// Launch hidden in the tray when Windows starts.
    pub start_with_windows: bool,
    /// Show a notification when a download finishes or fails.
    pub notify_on_complete: bool,
    /// Accept downloads sent by the browser extension.
    pub browser_capture: bool,
    /// Only download during these hours.
    pub schedule: Schedule,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| PathBuf::from(".")),
            max_concurrent: 3,
            connections: 8,
            speed_limit: None,
            minimize_to_tray: false,
            start_with_windows: false,
            notify_on_complete: true,
            browser_capture: true,
            schedule: Schedule::default(),
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
    pub expected_sha256: Option<String>,
    pub checksum: Option<Checksum>,
    /// Queued, but outside the scheduled hours.
    pub waiting_for_schedule: bool,
}
