use crate::fsutil;
use crate::headers;
use crate::job::{self, Cancelled, Job, StopReason};
use crate::limit::RateLimiter;
use crate::naming::{self, PART_EXT};
use crate::probe;
use crate::types::*;
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use url::Url;

// An honest UA on purpose: pretending to be Chrome without Chrome's TLS
// fingerprint gets connections dropped by bot protection on many CDNs.
const USER_AGENT: &str = concat!("GimmeThatFile/", env!("CARGO_PKG_VERSION"));

/// Handle to the download manager. Cheap to clone.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    client: Client,
    state: Mutex<State>,
    store: PathBuf,
    save_lock: Mutex<()>,
    rt: Handle,
    version: AtomicU64,
    events: broadcast::Sender<Event>,
    /// Global speed limit, shared by every running download.
    global_limit: Arc<RateLimiter>,
}

struct State {
    settings: Settings,
    entries: Vec<Entry>,
    next_id: DownloadId,
    /// Inside the scheduled hours (always true without a schedule).
    schedule_open: bool,
}

struct Entry {
    rec: DownloadRecord,
    /// Present while the download is running.
    job: Option<Arc<Job>>,
    /// Started by the user with "Start now": ignores the schedule.
    forced: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Persisted {
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    downloads: Vec<DownloadRecord>,
    #[serde(default)]
    next_id: DownloadId,
}

/// HTTP client with the engine's defaults plus `extra` request headers,
/// which are then sent with every request made through it (including
/// redirects to the same host; reqwest drops Cookie/Authorization when a
/// redirect leaves the host).
fn build_client(extra: &[(String, String)]) -> Result<Client> {
    let mut defaults = reqwest::header::HeaderMap::new();
    defaults.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(USER_AGENT),
    );
    // A browser-provided User-Agent replaces ours.
    defaults.extend(headers::to_map(extra));
    Ok(Client::builder()
        .default_headers(defaults)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(32)
        .tcp_nodelay(true)
        .build()?)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// File name for messages, falling back to the URL.
fn display_name(rec: &DownloadRecord) -> String {
    rec.file_name.clone().unwrap_or_else(|| rec.url.clone())
}

/// Lowercase hex SHA-256 of a file.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn same_name(a: &str, b: &str) -> bool {
    if cfg!(windows) || cfg!(target_os = "macos") {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

impl State {
    fn get_mut(&mut self, id: DownloadId) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.rec.id == id)
    }
    fn name_taken(&self, dir: &Path, name: &str, except: DownloadId) -> bool {
        self.entries.iter().any(|e| {
            e.rec.id != except
                && e.rec.dir == dir
                && e.rec
                    .file_name
                    .as_deref()
                    .is_some_and(|n| same_name(n, name))
        })
    }
}

impl Manager {
    /// Load state from `store` (a JSON file) and start any queued downloads.
    /// Must be called from inside a Tokio runtime.
    pub fn load(store: impl Into<PathBuf>) -> Result<Self> {
        let store = store.into();
        let persisted: Persisted = match fs::read(&store) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                let _ = fs::copy(&store, store.with_extension("json.bak"));
                Persisted::default()
            }),
            Err(_) => Persisted::default(),
        };
        let next_id = persisted
            .downloads
            .iter()
            .map(|d| d.id + 1)
            .max()
            .unwrap_or(1)
            .max(persisted.next_id);
        let entries = persisted
            .downloads
            .into_iter()
            .map(|mut rec| {
                // Anything that was running when the app closed continues.
                if matches!(rec.status, Status::Connecting | Status::Downloading) {
                    rec.status = Status::Queued;
                }
                Entry {
                    rec,
                    job: None,
                    forced: false,
                }
            })
            .collect();

        let client = build_client(&[])?;

        let schedule_open = persisted.settings.schedule.allows_now();
        let global_limit = Arc::new(RateLimiter::new(persisted.settings.speed_limit));
        let inner = Arc::new(Inner {
            client,
            state: Mutex::new(State {
                settings: persisted.settings,
                entries,
                next_id,
                schedule_open,
            }),
            store,
            save_lock: Mutex::new(()),
            rt: Handle::current(),
            version: AtomicU64::new(1),
            events: broadcast::channel(64).0,
            global_limit,
        });
        Inner::spawn_ticker(&inner);
        inner.schedule();
        // Finish checksum checks interrupted by closing the app.
        let unverified: Vec<DownloadId> = inner
            .state
            .lock()
            .entries
            .iter()
            .filter(|e| e.rec.checksum == Some(Checksum::Verifying))
            .map(|e| e.rec.id)
            .collect();
        for id in unverified {
            let inner = inner.clone();
            inner
                .rt
                .clone()
                .spawn(async move { inner.verify(id).await });
        }
        Ok(Self { inner })
    }

    /// Queue a new download. `dir`/`connections` fall back to the settings.
    pub fn add(
        &self,
        url: &str,
        dir: Option<PathBuf>,
        connections: Option<u32>,
    ) -> Result<DownloadId> {
        self.add_with(
            url,
            AddOptions {
                dir,
                connections,
                ..Default::default()
            },
        )
    }

    /// Queue a new download with extra options (headers, limits, ...).
    pub fn add_with(&self, url: &str, opts: AddOptions) -> Result<DownloadId> {
        let parsed = Url::parse(url.trim()).context("not a valid link")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("only http and https links are supported");
        }
        let expected_sha256 = match opts
            .expected_sha256
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
        {
            Some(h) if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) => {
                bail!("a SHA-256 hash is 64 hexadecimal characters")
            }
            h => h,
        };
        let id = {
            let mut st = self.inner.state.lock();
            let id = st.next_id;
            st.next_id += 1;
            let rec = DownloadRecord {
                id,
                url: parsed.to_string(),
                dir: opts.dir.unwrap_or_else(|| st.settings.download_dir.clone()),
                file_name: None,
                total: None,
                resumable: false,
                status: Status::Queued,
                error: None,
                segments: vec![],
                downloaded: 0,
                connections: opts
                    .connections
                    .unwrap_or(st.settings.connections)
                    .clamp(1, 32),
                etag: None,
                last_modified: None,
                added_at: now(),
                completed_at: None,
                name_hint: opts
                    .name_hint
                    .as_deref()
                    .map(naming::sanitize)
                    .filter(|n| n != "download"),
                headers: headers::sanitize(opts.headers),
                speed_limit: opts.speed_limit.filter(|&l| l > 0),
                expected_sha256,
                checksum: None,
            };
            st.entries.push(Entry {
                rec,
                job: None,
                forced: false,
            });
            id
        };
        self.inner.save();
        self.inner.schedule();
        Ok(id)
    }

    pub fn pause(&self, id: DownloadId) {
        {
            let mut st = self.inner.state.lock();
            let Some(e) = st.get_mut(id) else { return };
            e.forced = false;
            if let Some(job) = &e.job {
                job.stop(StopReason::Pause);
                e.rec.status = Status::Paused;
            } else if e.rec.status == Status::Queued {
                e.rec.status = Status::Paused;
            }
        }
        self.inner.bump();
        self.inner.save();
    }

    pub fn resume(&self, id: DownloadId) {
        {
            let mut st = self.inner.state.lock();
            let Some(e) = st.get_mut(id) else { return };
            if matches!(e.rec.status, Status::Paused | Status::Failed) {
                e.rec.status = Status::Queued;
                e.rec.error = None;
                e.forced = false;
            }
        }
        self.inner.schedule();
        self.inner.save();
    }

    /// Start (or resume) a download now, even outside the scheduled hours.
    /// It still waits for a free slot.
    pub fn force_start(&self, id: DownloadId) {
        {
            let mut st = self.inner.state.lock();
            let Some(e) = st.get_mut(id) else { return };
            if matches!(
                e.rec.status,
                Status::Queued | Status::Paused | Status::Failed
            ) {
                e.rec.status = Status::Queued;
                e.rec.error = None;
                e.forced = true;
            }
        }
        self.inner.schedule();
        self.inner.save();
    }

    /// Download events (completed, failed, checksum mismatch) for
    /// notifications. Slow receivers may miss events (`Lagged`).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn pause_all(&self) {
        let ids = self
            .ids_where(|s| matches!(s, Status::Queued | Status::Connecting | Status::Downloading));
        ids.into_iter().for_each(|id| self.pause(id));
    }

    pub fn resume_all(&self) {
        let ids = self.ids_where(|s| matches!(s, Status::Paused | Status::Failed));
        ids.into_iter().for_each(|id| self.resume(id));
    }

    fn ids_where(&self, f: impl Fn(Status) -> bool) -> Vec<DownloadId> {
        let st = self.inner.state.lock();
        st.entries
            .iter()
            .filter(|e| f(e.rec.status))
            .map(|e| e.rec.id)
            .collect()
    }

    /// Remove from the list. With `delete_file`, also delete what's on disk.
    pub fn remove(&self, id: DownloadId, delete_file: bool) {
        let entry = {
            let mut st = self.inner.state.lock();
            let Some(idx) = st.entries.iter().position(|e| e.rec.id == id) else {
                return;
            };
            st.entries.remove(idx)
        };
        if let Some(job) = &entry.job {
            // The job deletes its own partial file once it has closed it.
            job.stop(if delete_file {
                StopReason::RemoveDelete
            } else {
                StopReason::Remove
            });
        } else if delete_file {
            if let Some(name) = &entry.rec.file_name {
                if entry.rec.status == Status::Completed {
                    let _ = fs::remove_file(entry.rec.dir.join(name));
                } else {
                    let _ = fs::remove_file(entry.rec.dir.join(format!("{name}{PART_EXT}")));
                }
            }
        }
        self.inner.bump();
        self.inner.save();
        self.inner.schedule();
    }

    /// Drop finished downloads from the list (files stay on disk).
    pub fn clear_completed(&self) {
        self.inner
            .state
            .lock()
            .entries
            .retain(|e| e.rec.status != Status::Completed);
        self.inner.bump();
        self.inner.save();
    }

    pub fn settings(&self) -> Settings {
        self.inner.state.lock().settings.clone()
    }

    pub fn set_settings(&self, mut s: Settings) {
        s.max_concurrent = s.max_concurrent.clamp(1, 10);
        s.connections = s.connections.clamp(1, 32);
        s.speed_limit = s.speed_limit.filter(|&l| l > 0);
        // Applies to running downloads right away.
        self.inner.global_limit.set_rate(s.speed_limit);
        self.inner.state.lock().settings = s;
        self.inner.bump();
        self.inner.save();
        self.inner.apply_schedule();
        self.inner.schedule();
    }

    /// Change one download's speed limit (bytes/second, `None` or 0 =
    /// unlimited). Takes effect immediately if it is running.
    pub fn set_speed_limit(&self, id: DownloadId, limit: Option<u64>) {
        let limit = limit.filter(|&l| l > 0);
        {
            let mut st = self.inner.state.lock();
            let Some(e) = st.get_mut(id) else { return };
            e.rec.speed_limit = limit;
            if let Some(job) = &e.job {
                job.limit.set_rate(limit);
            }
        }
        self.inner.bump();
        self.inner.save();
    }

    /// Where the file is (or will be) on disk.
    pub fn file_path(&self, id: DownloadId) -> Option<(PathBuf, Status)> {
        let st = self.inner.state.lock();
        let e = st.entries.iter().find(|e| e.rec.id == id)?;
        let path = match &e.rec.file_name {
            Some(name) if e.rec.status == Status::Completed => e.rec.dir.join(name),
            Some(name) => e.rec.dir.join(format!("{name}{PART_EXT}")),
            None => e.rec.dir.clone(),
        };
        Some((path, e.rec.status))
    }

    /// Changes whenever something other than live progress changes.
    pub fn version(&self) -> u64 {
        self.inner.version.load(Relaxed)
    }

    pub fn has_active(&self) -> bool {
        self.inner
            .state
            .lock()
            .entries
            .iter()
            .any(|e| e.job.is_some())
    }

    pub fn snapshot(&self) -> Vec<Snapshot> {
        let st = self.inner.state.lock();
        let open = st.schedule_open;
        st.entries
            .iter()
            .map(|e| {
                let r = &e.rec;
                let (downloaded, segments, speed, connections) = match &e.job {
                    Some(j) if j.is_initialized() => (
                        j.downloaded.load(Relaxed),
                        j.ranges(),
                        j.speed.lock().value,
                        j.workers.load(Relaxed),
                    ),
                    Some(j) => (
                        r.downloaded,
                        r.segments.clone(),
                        0.0,
                        j.workers.load(Relaxed),
                    ),
                    None => (r.downloaded, r.segments.clone(), 0.0, 0),
                };
                let eta = match r.total {
                    Some(t) if speed > 1.0 && e.job.is_some() => {
                        Some(t.saturating_sub(downloaded) as f64 / speed)
                    }
                    _ => None,
                };
                Snapshot {
                    id: r.id,
                    url: r.url.clone(),
                    file_name: r.file_name.clone(),
                    dir: r.dir.display().to_string(),
                    total: r.total,
                    downloaded,
                    speed,
                    eta,
                    status: r.status,
                    error: r.error.clone(),
                    resumable: r.resumable,
                    connections,
                    max_connections: r.connections,
                    segments: if r.resumable { segments } else { vec![] },
                    speed_limit: r.speed_limit,
                    added_at: r.added_at,
                    completed_at: r.completed_at,
                    expected_sha256: r.expected_sha256.clone(),
                    checksum: r.checksum,
                    waiting_for_schedule: r.status == Status::Queued && !open && !e.forced,
                }
            })
            .collect()
    }

    /// Call on app exit: records exact progress, then stops everything.
    pub fn shutdown(&self) {
        {
            let mut st = self.inner.state.lock();
            for e in st.entries.iter_mut() {
                if let Some(job) = e.job.take() {
                    if job.is_initialized() && e.rec.resumable {
                        e.rec.segments = job.ranges();
                        e.rec.downloaded = job.downloaded.load(Relaxed);
                    }
                    if e.rec.status != Status::Paused {
                        e.rec.status = Status::Queued;
                    }
                    job.stop(StopReason::Shutdown);
                }
            }
        }
        self.inner.save();
    }
}

impl Inner {
    fn bump(&self) {
        self.version.fetch_add(1, Relaxed);
    }

    fn record(&self, id: DownloadId) -> Option<DownloadRecord> {
        let st = self.state.lock();
        st.entries
            .iter()
            .find(|e| e.rec.id == id)
            .map(|e| e.rec.clone())
    }

    fn save(&self) {
        let _guard = self.save_lock.lock();
        let data = {
            let st = self.state.lock();
            let downloads = st
                .entries
                .iter()
                .map(|e| {
                    let mut rec = e.rec.clone();
                    if let Some(j) = &e.job {
                        if j.is_initialized() && rec.resumable {
                            rec.segments = j.ranges();
                            rec.downloaded = j.downloaded.load(Relaxed);
                        }
                    }
                    rec
                })
                .collect();
            Persisted {
                settings: st.settings.clone(),
                downloads,
                next_id: st.next_id,
            }
        };
        let Ok(json) = serde_json::to_vec_pretty(&data) else {
            return;
        };
        if let Some(parent) = self.store.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = self.store.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &self.store);
        }
    }

    /// Start queued downloads while there are free slots.
    fn schedule(self: &Arc<Self>) {
        let to_start: Vec<(DownloadId, Arc<Job>)> = {
            let mut st = self.state.lock();
            let max = st.settings.max_concurrent.max(1) as usize;
            let mut active = st.entries.iter().filter(|e| e.job.is_some()).count();
            let open = st.schedule_open;
            let mut out = vec![];
            for e in st.entries.iter_mut() {
                if active >= max {
                    break;
                }
                if e.rec.status == Status::Queued && e.job.is_none() && (open || e.forced) {
                    let job = Job::with_global_limit(self.global_limit.clone());
                    job.limit.set_rate(e.rec.speed_limit);
                    e.job = Some(job.clone());
                    e.rec.status = Status::Connecting;
                    e.rec.error = None;
                    out.push((e.rec.id, job));
                    active += 1;
                }
            }
            out
        };
        for (id, job) in to_start {
            let inner = self.clone();
            self.rt.spawn(async move { inner.run(id, job).await });
        }
        self.bump();
    }

    /// Re-check the scheduled hours. When they end, running downloads go back
    /// to the queue (keeping their progress); when they start, the queue runs.
    fn apply_schedule(self: &Arc<Self>) {
        let (open, changed) = {
            let mut st = self.state.lock();
            let open = st.settings.schedule.allows_now();
            let changed = open != st.schedule_open;
            st.schedule_open = open;
            if changed && !open {
                for e in st.entries.iter_mut().filter(|e| !e.forced) {
                    if let Some(job) = &e.job {
                        // `run` keeps a `Queued` status after a pause-stop,
                        // so the download is restarted, not shown as paused.
                        e.rec.status = Status::Queued;
                        job.stop(StopReason::Pause);
                    }
                }
            }
            (open, changed)
        };
        if changed {
            self.bump();
            self.save();
            if open {
                self.schedule();
            }
        }
    }

    fn spawn_ticker(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.rt.spawn(async move {
            let mut tick = 0u64;
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let Some(inner) = weak.upgrade() else { break };
                tick += 1;
                inner.apply_schedule();
                let any = {
                    let st = inner.state.lock();
                    let mut any = false;
                    for e in st.entries.iter() {
                        if let Some(j) = &e.job {
                            any = true;
                            j.speed.lock().sample(j.received.load(Relaxed));
                        }
                    }
                    any
                };
                // Persist resume points every few seconds while downloading.
                if any && tick.is_multiple_of(6) {
                    inner.save();
                }
            }
        });
    }

    async fn run(self: Arc<Self>, id: DownloadId, job: Arc<Job>) {
        let result = self.drive(id, &job).await;
        let reason = job.reason();
        let mut event = None;
        let mut verify = false;
        let removed = {
            let mut st = self.state.lock();
            match st.get_mut(id) {
                // A shutdown already recorded the state and detached the job.
                Some(_) if reason == StopReason::Shutdown => false,
                Some(e) => {
                    e.job = None;
                    if job.is_initialized() {
                        e.rec.segments = job.ranges();
                        e.rec.downloaded = job.downloaded.load(Relaxed);
                    }
                    match &result {
                        Ok(()) => {
                            e.rec.status = Status::Completed;
                            e.rec.error = None;
                            e.rec.segments.clear();
                            if let Some(t) = e.rec.total {
                                e.rec.downloaded = t;
                            }
                            e.rec.total.get_or_insert(e.rec.downloaded);
                            e.rec.completed_at = Some(now());
                            if e.rec.expected_sha256.is_some() {
                                e.rec.checksum = Some(Checksum::Verifying);
                                verify = true;
                            } else {
                                event = Some(Event::Completed {
                                    id,
                                    file_name: display_name(&e.rec),
                                });
                            }
                        }
                        Err(err) => {
                            if reason == StopReason::Pause || err.is::<Cancelled>() {
                                // The user may have hit resume before we got here.
                                if e.rec.status != Status::Queued {
                                    e.rec.status = Status::Paused;
                                }
                            } else {
                                e.rec.status = Status::Failed;
                                e.rec.error = Some(format!("{err:#}"));
                                event = Some(Event::Failed {
                                    id,
                                    file_name: display_name(&e.rec),
                                    error: format!("{err:#}"),
                                });
                            }
                            if !e.rec.resumable {
                                e.rec.segments.clear();
                                e.rec.downloaded = 0;
                            }
                        }
                    }
                    false
                }
                None => true,
            }
        };
        if removed && reason == StopReason::RemoveDelete {
            if let Some(p) = job.part_path.lock().clone() {
                let _ = fs::remove_file(p);
            }
        }
        if reason != StopReason::Shutdown {
            self.save();
            self.schedule();
        }
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        if verify {
            self.verify(id).await;
        }
    }

    /// Hash the finished file (on a blocking thread) and compare it with
    /// `expected_sha256`. A mismatch keeps the download `Completed` but marks
    /// the checksum, so the file stays available for the user to inspect.
    async fn verify(self: &Arc<Self>, id: DownloadId) {
        let Some(rec) = self.record(id) else { return };
        let (Some(expected), Some(name)) = (rec.expected_sha256.clone(), rec.file_name.clone())
        else {
            return;
        };
        let path = rec.dir.join(&name);
        let hashed = tokio::task::spawn_blocking(move || sha256_file(&path)).await;
        let result = match hashed {
            Ok(Ok(actual)) if actual == expected => Checksum::Ok,
            _ => Checksum::Mismatch,
        };
        let event = {
            let mut st = self.state.lock();
            let Some(e) = st.get_mut(id) else { return };
            if e.rec.checksum != Some(Checksum::Verifying) {
                return;
            }
            e.rec.checksum = Some(result);
            let file_name = display_name(&e.rec);
            match result {
                Checksum::Ok => Event::Completed { id, file_name },
                _ => Event::ChecksumMismatch { id, file_name },
            }
        };
        self.bump();
        self.save();
        let _ = self.events.send(event);
    }

    async fn drive(self: &Arc<Self>, id: DownloadId, job: &Arc<Job>) -> Result<()> {
        let rec = self.record(id).ok_or(Cancelled)?;
        // Downloads with their own headers (cookies, ...) get their own
        // client so those headers go out with every request; the rest share one.
        let client = if rec.headers.is_empty() {
            self.client.clone()
        } else {
            if std::env::var_os("GTF_DEBUG").is_some() {
                // Names only: values may be cookies or tokens.
                let names: Vec<_> = rec.headers.iter().map(|(n, _)| n.as_str()).collect();
                eprintln!("[gtf] download {id}: extra headers {names:?}");
            }
            build_client(&rec.headers)?
        };
        let mut attempt = 0u64;
        let info = loop {
            let res = tokio::select! {
                _ = job.cancel.cancelled() => return Err(Cancelled.into()),
                r = probe::probe(&client, &rec.url) => r,
            };
            match res {
                Ok(info) => break info,
                // Network-level errors are often transient; HTTP errors are not.
                Err(e) if attempt < 2 && e.is::<reqwest::Error>() => {
                    attempt += 1;
                    tokio::select! {
                        _ = job.cancel.cancelled() => return Err(Cancelled.into()),
                        _ = tokio::time::sleep(Duration::from_secs(attempt * 2)) => {}
                    }
                }
                Err(e) => return Err(e),
            }
        };
        fs::create_dir_all(&rec.dir)
            .with_context(|| format!("cannot create folder {}", rec.dir.display()))?;

        let ranged = info.resumable && info.total.is_some();
        let part_of = |name: &str| rec.dir.join(format!("{name}{PART_EXT}"));
        let can_resume = ranged
            && rec.resumable
            && !rec.segments.is_empty()
            && rec.total == info.total
            && match (&rec.etag, &info.etag) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
            && rec
                .file_name
                .as_deref()
                .is_some_and(|n| part_of(n).exists());

        let file_name = {
            let mut st = self.state.lock();
            let name = match &rec.file_name {
                Some(n) => n.clone(),
                None => naming::unique(
                    &rec.dir,
                    rec.name_hint.as_deref().unwrap_or(&info.file_name),
                    |c| st.name_taken(&rec.dir, c, id),
                ),
            };
            let Some(e) = st.get_mut(id) else {
                return Err(Cancelled.into());
            };
            e.rec.file_name = Some(name.clone());
            e.rec.total = info.total;
            e.rec.resumable = ranged;
            e.rec.etag = info.etag.clone();
            e.rec.last_modified = info.last_modified.clone();
            if e.rec.status == Status::Connecting {
                e.rec.status = Status::Downloading;
            }
            if !can_resume {
                e.rec.segments = match info.total {
                    Some(t) if ranged => job::initial_ranges(t, e.rec.connections),
                    _ => vec![],
                };
                e.rec.downloaded = 0;
            }
            name
        };
        self.bump();

        let part = part_of(&file_name);
        *job.part_path.lock() = Some(part.clone());
        let file = if can_resume {
            OpenOptions::new().read(true).write(true).open(&part)?
        } else {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&part)
                .with_context(|| format!("cannot create {}", part.display()))?;
            if ranged {
                fsutil::set_sparse(&f, true);
                f.set_len(info.total.unwrap_or(0))?;
            }
            f
        };
        let file = Arc::new(file);
        let (segments, connections) = self
            .record(id)
            .map(|r| (r.segments, r.connections))
            .ok_or(Cancelled)?;
        job.init(segments, info.total);
        self.save();

        if ranged {
            job::download_ranged(
                client.clone(),
                rec.url.clone(),
                file.clone(),
                job.clone(),
                connections,
            )
            .await?;
            fsutil::set_sparse(&file, false);
        } else {
            job::download_single(client.clone(), rec.url.clone(), file.clone(), job.clone())
                .await?;
        }
        file.sync_all().ok();
        drop(file);

        let final_name = {
            let st = self.state.lock();
            if rec.dir.join(&file_name).exists() {
                naming::unique(&rec.dir, &file_name, |c| st.name_taken(&rec.dir, c, id))
            } else {
                file_name.clone()
            }
        };
        fs::rename(&part, rec.dir.join(&final_name))
            .with_context(|| format!("cannot rename {}", part.display()))?;
        if let Some(e) = self.state.lock().get_mut(id) {
            e.rec.file_name = Some(final_name);
        }
        Ok(())
    }
}
