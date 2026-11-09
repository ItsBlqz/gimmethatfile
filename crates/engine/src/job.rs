//! A single running download.
//!
//! The file is split into segments. Each connection ("worker") claims a
//! segment and streams its byte range straight into the file at the right
//! offset. When a worker runs out of work it steals the back half of the
//! largest remaining segment, so all connections stay busy until the end
//! (the trick IDM calls "dynamic segmentation").

use crate::control::{Controller, Sample};
use crate::fsutil::write_all_at;
use crate::limit::RateLimiter;
use crate::probe::parse_range_start;
use crate::types::Range;
use anyhow::anyhow;
use parking_lot::Mutex;
use reqwest::{header, Client, StatusCode};
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

macro_rules! debug {
    ($($t:tt)*) => {
        if std::env::var_os("GTF_DEBUG").is_some() {
            eprintln!("[gtf] {}", format!($($t)*));
        }
    };
}

/// Never split a segment into pieces smaller than this.
pub const MIN_SPLIT: u64 = 1 << 20;
/// Near the end of a download, allow smaller splits so the last few
/// megabytes don't hang on one slow connection while the others sit idle.
const TAIL_SPLIT: u64 = 256 << 10;
/// "Near the end" = this much or less is left in total.
const TAIL_ZONE: u64 = 16 << 20;

/// Smallest segment worth splitting in half, given how much is left overall.
fn split_min(total_remaining: u64) -> u64 {
    if total_remaining <= TAIL_ZONE {
        TAIL_SPLIT
    } else {
        MIN_SPLIT
    }
}

/// Bytes left across all segments (caller holds the segment-list lock).
fn remaining_of(segs: &[Arc<Segment>]) -> u64 {
    segs.iter()
        .map(|s| {
            let (p, e) = s.range();
            e.saturating_sub(p)
        })
        .sum()
}
/// Bytes buffered per connection before they are written to disk.
const BUF_SIZE: usize = 512 * 1024;
/// Write at least this often so progress (and resume state) stays fresh.
const FLUSH_EVERY: Duration = Duration::from_millis(500);
/// Consecutive failures without progress before a connection gives up.
const MAX_SEG_FAILURES: u32 = 6;
/// Fail if not a single byte arrives for this long.
const STALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Minimum gap between opening two connections (servers dislike bursts).
const SPAWN_GAP: Duration = Duration::from_millis(150);

#[derive(Debug)]
pub struct Cancelled;
impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}
impl std::error::Error for Cancelled {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum StopReason {
    None = 0,
    Pause = 1,
    Remove = 2,
    RemoveDelete = 3,
    Shutdown = 4,
}

pub struct Segment {
    st: Mutex<SegState>,
}

struct SegState {
    pos: u64,
    end: u64,
    /// A worker is currently downloading this segment.
    owned: bool,
}

impl Segment {
    fn new(pos: u64, end: u64, owned: bool) -> Arc<Self> {
        Arc::new(Self {
            st: Mutex::new(SegState { pos, end, owned }),
        })
    }
    fn range(&self) -> (u64, u64) {
        let s = self.st.lock();
        (s.pos, s.end)
    }
    fn release(&self) {
        self.st.lock().owned = false;
    }
}

pub struct SpeedMeter {
    last_bytes: u64,
    last_at: Instant,
    pub value: f64,
}

impl SpeedMeter {
    pub fn sample(&mut self, bytes: u64) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_at).as_secs_f64();
        if dt < 0.2 {
            return;
        }
        let instant = bytes.saturating_sub(self.last_bytes) as f64 / dt;
        // Exponential moving average keeps the number from jumping around.
        self.value = if self.value == 0.0 {
            instant
        } else {
            self.value * 0.7 + instant * 0.3
        };
        self.last_bytes = bytes;
        self.last_at = now;
    }
}

pub struct Job {
    pub cancel: CancellationToken,
    reason: AtomicU8,
    /// Bytes received from the network (drives the speed meter).
    pub received: AtomicU64,
    /// Bytes safely written to disk.
    pub downloaded: AtomicU64,
    pub workers: AtomicU32,
    /// How many connections we currently aim for (see `control.rs`).
    target: AtomicU32,
    /// Times the server pushed back (429/503/...); read by the supervisor.
    throttles: AtomicU32,
    /// This download's own speed limit.
    pub limit: RateLimiter,
    /// Limit shared by all downloads (owned by the manager).
    global_limit: Arc<RateLimiter>,
    /// Total time connections spent waiting for a speed limiter, in ns.
    limiter_wait_ns: AtomicU64,
    initialized: AtomicBool,
    segments: Mutex<Vec<Arc<Segment>>>,
    pub part_path: Mutex<Option<PathBuf>>,
    pub speed: Mutex<SpeedMeter>,
    fatal: Mutex<Option<String>>,
}

impl Job {
    #[allow(dead_code)] // the manager uses `with_global_limit`; kept for tests
    pub fn new() -> Arc<Self> {
        Self::with_global_limit(Arc::new(RateLimiter::default()))
    }

    /// A job whose connections also obey `global` (shared with other jobs).
    pub fn with_global_limit(global: Arc<RateLimiter>) -> Arc<Self> {
        Arc::new(Self {
            cancel: CancellationToken::new(),
            reason: AtomicU8::new(StopReason::None as u8),
            received: AtomicU64::new(0),
            downloaded: AtomicU64::new(0),
            workers: AtomicU32::new(0),
            target: AtomicU32::new(1),
            throttles: AtomicU32::new(0),
            limit: RateLimiter::default(),
            global_limit: global,
            limiter_wait_ns: AtomicU64::new(0),
            initialized: AtomicBool::new(false),
            segments: Mutex::new(Vec::new()),
            part_path: Mutex::new(None),
            speed: Mutex::new(SpeedMeter {
                last_bytes: 0,
                last_at: Instant::now(),
                value: 0.0,
            }),
            fatal: Mutex::new(None),
        })
    }

    pub fn stop(&self, reason: StopReason) {
        self.reason.store(reason as u8, Relaxed);
        self.cancel.cancel();
    }

    pub fn reason(&self) -> StopReason {
        match self.reason.load(Relaxed) {
            1 => StopReason::Pause,
            2 => StopReason::Remove,
            3 => StopReason::RemoveDelete,
            4 => StopReason::Shutdown,
            _ => StopReason::None,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Relaxed)
    }

    /// Load the ranges that still need downloading.
    pub fn init(&self, ranges: Vec<Range>, total: Option<u64>) {
        let segs: Vec<_> = ranges
            .into_iter()
            .filter(|r| r.pos < r.end)
            .map(|r| Segment::new(r.pos, r.end, false))
            .collect();
        *self.segments.lock() = segs;
        let done = match total {
            Some(t) => t.saturating_sub(self.remaining()),
            None => 0,
        };
        self.downloaded.store(done, Relaxed);
        self.initialized.store(true, Relaxed);
    }

    pub fn ranges(&self) -> Vec<Range> {
        let mut v: Vec<Range> = self
            .segments
            .lock()
            .iter()
            .map(|s| s.range())
            .filter(|(p, e)| p < e)
            .map(|(pos, end)| Range { pos, end })
            .collect();
        v.sort_by_key(|r| r.pos);
        v
    }

    pub fn remaining(&self) -> u64 {
        self.segments
            .lock()
            .iter()
            .map(|s| {
                let (p, e) = s.range();
                e.saturating_sub(p)
            })
            .sum()
    }

    fn release_all(&self) {
        for s in self.segments.lock().iter() {
            s.release();
        }
    }

    /// `seg` is finished. If the segment right after it is idle, append it
    /// to `seg` so the worker can keep reading its open connection.
    fn extend(&self, seg: &Arc<Segment>) -> bool {
        let segs = self.segments.lock();
        let end = {
            let me = seg.st.lock();
            if me.pos < me.end {
                return false;
            }
            me.end
        };
        for s in segs.iter() {
            if Arc::ptr_eq(s, seg) {
                continue;
            }
            let mut st = s.st.lock();
            if !st.owned && st.pos == end && st.pos < st.end {
                let new_end = st.end;
                st.pos = st.end;
                drop(st);
                seg.st.lock().end = new_end;
                return true;
            }
        }
        false
    }

    fn has_claimable(&self) -> bool {
        let segs = self.segments.lock();
        let min = split_min(remaining_of(&segs));
        segs.iter().any(|s| {
            let st = s.st.lock();
            let rem = st.end - st.pos;
            (!st.owned && rem > 0) || (st.owned && rem >= 2 * min)
        })
    }

    /// Let `n` received bytes through the speed limits, sleeping as needed.
    /// Returns false if `token` was cancelled while waiting.
    async fn pace(&self, n: usize, token: &CancellationToken) -> bool {
        if !self.limit.is_limited() && !self.global_limit.is_limited() {
            return true;
        }
        let n = n as u64;
        let limits = [&self.limit, &*self.global_limit];
        let wait = crate::limit::acquire_all(&limits, n);
        tokio::select! {
            _ = token.cancelled() => false,
            waited = wait => {
                if !waited.is_zero() {
                    self.limiter_wait_ns
                        .fetch_add(waited.as_nanos() as u64, Relaxed);
                }
                true
            }
        }
    }

    /// `n` received bytes were thrown away (they overlapped another
    /// segment). Don't count them as speed, and give them back to the
    /// limiters so the limit applies to useful bytes.
    fn discard(&self, n: usize) {
        if n == 0 {
            return;
        }
        let n = n as u64;
        // They were added to `received` when they arrived, so no underflow.
        self.received.fetch_sub(n, Relaxed);
        self.limit.refund(n);
        self.global_limit.refund(n);
    }

    /// More connections are open than the target: atomically take this one
    /// off the count. The caller must then stop without decrementing again.
    fn try_retire(&self) -> bool {
        let target = self.target.load(Relaxed).max(1);
        let mut w = self.workers.load(Relaxed);
        while w > target {
            match self
                .workers
                .compare_exchange_weak(w, w - 1, Relaxed, Relaxed)
            {
                Ok(_) => return true,
                Err(now) => w = now,
            }
        }
        false
    }

    fn set_fatal(&self, e: &anyhow::Error) {
        self.fatal.lock().get_or_insert_with(|| format!("{e:#}"));
    }

    /// Give a worker something to do: an unowned segment if there is one,
    /// otherwise the back half of the biggest segment someone else is on.
    fn claim(&self) -> Option<Arc<Segment>> {
        let mut segs = self.segments.lock();
        segs.retain(|s| {
            let (p, e) = s.range();
            p < e
        });
        for s in segs.iter() {
            let mut st = s.st.lock();
            if !st.owned && st.pos < st.end {
                st.owned = true;
                return Some(s.clone());
            }
        }
        let min = split_min(remaining_of(&segs));
        let victim = segs
            .iter()
            .filter_map(|s| {
                let st = s.st.lock();
                let rem = st.end - st.pos;
                (st.owned && rem >= 2 * min).then_some((s.clone(), rem))
            })
            .max_by_key(|(_, rem)| *rem)?
            .0;
        let mut st = victim.st.lock();
        let rem = st.end - st.pos;
        if rem < 2 * min {
            return None;
        }
        let mid = st.pos + rem / 2;
        let stolen = Segment::new(mid, st.end, true);
        // The victim notices its shorter `end` on its next write and stops there.
        st.end = mid;
        drop(st);
        segs.push(stolen.clone());
        Some(stolen)
    }
}

/// Split `[0, total)` into `n` roughly equal ranges.
pub fn initial_ranges(total: u64, connections: u32) -> Vec<Range> {
    if total == 0 {
        return vec![];
    }
    let n = (connections.max(1) as u64)
        .min(total.div_ceil(MIN_SPLIT))
        .max(1);
    let size = total / n;
    (0..n)
        .map(|i| Range {
            pos: i * size,
            end: if i == n - 1 { total } else { (i + 1) * size },
        })
        .collect()
}

struct Ctx {
    client: Client,
    url: String,
    file: Arc<File>,
    job: Arc<Job>,
    token: CancellationToken,
}

enum FetchErr {
    /// Network hiccup: try again.
    Retry(anyhow::Error),
    /// Server is limiting connections: drop this one if others are alive.
    Throttled(anyhow::Error),
    /// Nothing will fix this (404, disk full, ...).
    Fatal(anyhow::Error),
}

enum FetchOk {
    Done,
    Cancelled,
    /// Too many connections are open; this one closed (see `try_retire`).
    Retired,
}

/// What a worker does after a segment.
enum Next {
    /// Claim more work.
    More,
    /// Stop (cancelled, throttled or fatal error).
    Stop,
    /// Stop; `try_retire` already took this worker off the count.
    Retired,
}

impl Ctx {
    /// Write buffered bytes into the segment's slot in the file. Returns true
    /// once the segment is complete. Bytes past `end` (because another worker
    /// stole that part) are dropped.
    fn commit(&self, seg: &Segment, buf: &mut Vec<u8>) -> Result<bool, FetchErr> {
        let mut st = seg.st.lock();
        let allowed = (st.end - st.pos).min(buf.len() as u64) as usize;
        if allowed > 0 {
            write_all_at(&self.file, &buf[..allowed], st.pos)
                .map_err(|e| FetchErr::Fatal(anyhow!("could not write to disk: {e}")))?;
            st.pos += allowed as u64;
            self.job.downloaded.fetch_add(allowed as u64, Relaxed);
        }
        buf.drain(..allowed);
        Ok(st.pos >= st.end)
    }

    /// Commit, and when the segment is finished try to keep streaming into
    /// the next idle segment on the same connection. Returns true when this
    /// connection has nothing left to do.
    fn commit_or_extend(&self, seg: &Arc<Segment>, buf: &mut Vec<u8>) -> Result<bool, FetchErr> {
        loop {
            if !self.commit(seg, buf)? {
                buf.clear();
                return Ok(false);
            }
            if !self.job.extend(seg) {
                // Whatever is left belongs to someone else's segment.
                self.job.discard(buf.len());
                buf.clear();
                return Ok(true);
            }
            if buf.is_empty() {
                return Ok(false);
            }
        }
    }
}

/// Download every remaining range using up to `connections` connections.
///
/// A small supervisor loop keeps the right number of workers alive. The
/// number comes from [`Controller`]: it measures what adding or removing
/// connections does to the total speed, backs off when the server pushes
/// back (429/503), and uses few connections when a speed limit (not the
/// network) is the bottleneck.
pub async fn download_ranged(
    client: Client,
    url: String,
    file: Arc<File>,
    job: Arc<Job>,
    connections: u32,
) -> anyhow::Result<()> {
    let mut ctl = Controller::new(connections, Instant::now());
    job.target.store(ctl.target(), Relaxed);
    job.release_all();
    let token = job.cancel.child_token();
    let ctx = Arc::new(Ctx {
        client,
        url,
        file,
        job: job.clone(),
        token: token.clone(),
    });

    let mut set = tokio::task::JoinSet::new();
    let mut last_err: Option<anyhow::Error> = None;
    let mut progress = (job.downloaded.load(Relaxed), Instant::now());
    let mut next_spawn = Instant::now();
    let mut throttles = 0;
    let mut tick = tokio::time::interval(SPAWN_GAP);

    let result = loop {
        tokio::select! {
            _ = token.cancelled() => {}
            Some(res) = set.join_next(), if !set.is_empty() => match res {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    // A connection gave up after several retries; wait before replacing it.
                    last_err = Some(e);
                    next_spawn = Instant::now() + Duration::from_secs(2);
                }
                Err(e) => last_err = Some(anyhow!("connection task crashed: {e}")),
            },
            _ = tick.tick() => {}
        }

        if let Some(f) = job.fatal.lock().take() {
            break Err(anyhow!(f));
        }
        if job.cancel.is_cancelled() {
            break Err(Cancelled.into());
        }
        if job.remaining() == 0 {
            break Ok(());
        }

        let now = Instant::now();
        let done = job.downloaded.load(Relaxed);
        if done > progress.0 {
            progress = (done, now);
        } else if now.duration_since(progress.1) > STALL_TIMEOUT {
            break Err(last_err
                .take()
                .unwrap_or_else(|| anyhow!("download stalled")));
        }

        // A worker that got a 429/503 already lowered `target`; tell the
        // controller so it doesn't raise it again right away.
        let t = job.throttles.load(Relaxed);
        if t != throttles {
            throttles = t;
            ctl.on_throttle(now, job.target.load(Relaxed));
        }
        // In the last few MB, measurements are dominated by connections
        // finishing, so stop adapting and keep the current target.
        let target = if job.remaining() <= TAIL_ZONE {
            job.target.load(Relaxed)
        } else {
            ctl.tick(Sample {
                at: now,
                bytes: job.received.load(Relaxed),
                workers: job.workers.load(Relaxed),
                limiter_wait: Duration::from_nanos(job.limiter_wait_ns.load(Relaxed)),
            })
        };
        job.target.store(target, Relaxed);

        if (set.len() as u32) < target && now >= next_spawn && job.has_claimable() {
            set.spawn(worker(ctx.clone()));
            next_spawn = now + SPAWN_GAP;
        }
    };

    // Let every worker flush its buffer and close the file handle.
    token.cancel();
    while set.join_next().await.is_some() {}
    result
}

async fn worker(ctx: Arc<Ctx>) -> anyhow::Result<()> {
    struct Count<'a>(&'a AtomicU32);
    impl Drop for Count<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Relaxed);
        }
    }
    ctx.job.workers.fetch_add(1, Relaxed);
    let count = Count(&ctx.job.workers);

    while !ctx.token.is_cancelled() {
        let Some(seg) = ctx.job.claim() else { break };
        match fetch_segment(&ctx, &seg).await? {
            Next::More => {}
            Next::Stop => break,
            Next::Retired => {
                // Already counted out by `try_retire`.
                std::mem::forget(count);
                break;
            }
        }
    }
    Ok(())
}

async fn fetch_segment(ctx: &Ctx, seg: &Arc<Segment>) -> anyhow::Result<Next> {
    let mut failures = 0u32;
    loop {
        if ctx.token.is_cancelled() {
            seg.release();
            return Ok(Next::Stop);
        }
        let (pos, end) = seg.range();
        if pos >= end {
            return Ok(Next::More);
        }
        let res = fetch_once(ctx, seg, pos, end).await;
        if let Err(FetchErr::Retry(e) | FetchErr::Throttled(e) | FetchErr::Fatal(e)) = &res {
            debug!(
                "segment @{pos}: {e:#} (moved {} bytes)",
                seg.range().0 - pos
            );
        }
        match res {
            Ok(FetchOk::Done) => return Ok(Next::More),
            Ok(FetchOk::Cancelled) => {
                seg.release();
                return Ok(Next::Stop);
            }
            Ok(FetchOk::Retired) => {
                seg.release();
                return Ok(Next::Retired);
            }
            Err(FetchErr::Fatal(e)) => {
                seg.release();
                ctx.job.set_fatal(&e);
                ctx.token.cancel();
                return Ok(Next::Stop);
            }
            Err(FetchErr::Throttled(_)) if ctx.job.workers.load(Relaxed) > 1 => {
                // The server doesn't want this many connections. Step back and
                // let the remaining workers pick up the segment later.
                let workers = ctx.job.workers.load(Relaxed);
                ctx.job
                    .target
                    .fetch_min(workers.saturating_sub(1).max(1), Relaxed);
                ctx.job.throttles.fetch_add(1, Relaxed);
                seg.release();
                return Ok(Next::Stop);
            }
            Err(err @ (FetchErr::Throttled(_) | FetchErr::Retry(_))) => {
                let (throttled, e) = match err {
                    FetchErr::Throttled(e) => (true, e),
                    FetchErr::Retry(e) | FetchErr::Fatal(e) => (false, e),
                };
                if seg.range().0 > pos {
                    failures = 0;
                } else {
                    failures += 1;
                }
                if failures >= MAX_SEG_FAILURES {
                    seg.release();
                    return Err(e);
                }
                let wait = if throttled {
                    Duration::from_millis(750 * failures.max(1) as u64)
                } else {
                    Duration::from_millis(500 * (1u64 << failures.min(5)))
                };
                tokio::select! {
                    _ = ctx.token.cancelled() => {
                        seg.release();
                        return Ok(Next::Stop);
                    }
                    _ = sleep(wait) => {}
                }
            }
        }
    }
}

fn classify(status: StatusCode) -> FetchErr {
    let e = anyhow!("server responded with {status}");
    match status.as_u16() {
        // A full-file reply to a mid-file range usually means the server is
        // overloaded or limiting connections; back off rather than give up.
        200 => FetchErr::Throttled(anyhow!(
            "server ignored the range request and sent the whole file"
        )),
        401 | 404 | 410 | 416 => FetchErr::Fatal(e),
        403 | 429 | 503 => FetchErr::Throttled(e),
        _ => FetchErr::Retry(e),
    }
}

async fn fetch_once(
    ctx: &Ctx,
    seg: &Arc<Segment>,
    pos: u64,
    end: u64,
) -> Result<FetchOk, FetchErr> {
    let req = ctx
        .client
        .get(&ctx.url)
        // Open-ended on purpose: when we reach `end` we may continue into the
        // next segment on this same connection instead of opening a new one.
        .header(header::RANGE, format!("bytes={pos}-"))
        .header(header::ACCEPT_ENCODING, "identity");
    let _ = end;
    let mut resp = tokio::select! {
        _ = ctx.token.cancelled() => return Ok(FetchOk::Cancelled),
        r = req.send() => r.map_err(|e| FetchErr::Retry(e.into()))?,
    };
    let status = resp.status();
    // `bytes=0-` asks for the entire file, and servers may legally answer
    // that with a plain 200. The body then starts at offset 0, which is
    // exactly what we want.
    let usable = status == StatusCode::PARTIAL_CONTENT || (status == StatusCode::OK && pos == 0);
    if !usable {
        return Err(classify(status));
    }
    let start = (status == StatusCode::PARTIAL_CONTENT)
        .then(|| {
            resp.headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_range_start)
        })
        .flatten();
    if start.is_some_and(|s| s != pos) {
        return Err(FetchErr::Retry(anyhow!("server returned the wrong range")));
    }

    let mut buf: Vec<u8> = Vec::with_capacity(BUF_SIZE);
    let mut last_flush = Instant::now();
    loop {
        let next = tokio::select! {
            _ = ctx.token.cancelled() => {
                ctx.commit(seg, &mut buf)?;
                return Ok(FetchOk::Cancelled);
            }
            c = resp.chunk() => c,
        };
        match next {
            Ok(Some(bytes)) => {
                ctx.job.received.fetch_add(bytes.len() as u64, Relaxed);
                buf.extend_from_slice(&bytes);
                if !ctx.job.pace(bytes.len(), &ctx.token).await {
                    ctx.commit(seg, &mut buf)?;
                    return Ok(FetchOk::Cancelled);
                }
                // Reached the end of the segment (possibly shortened by a
                // steal): commit now instead of reading on into bytes that
                // would be thrown away (and would count against a limit).
                let (pos, end) = seg.range();
                let at_end = pos + buf.len() as u64 >= end;
                if at_end || buf.len() >= BUF_SIZE || last_flush.elapsed() >= FLUSH_EVERY {
                    last_flush = Instant::now();
                    if ctx.commit_or_extend(seg, &mut buf)? {
                        return Ok(FetchOk::Done);
                    }
                    // Everything buffered is on disk now, so this is a
                    // clean point to close if there are too many connections.
                    if ctx.job.try_retire() {
                        return Ok(FetchOk::Retired);
                    }
                }
            }
            Ok(None) => {
                if ctx.commit_or_extend(seg, &mut buf)? {
                    return Ok(FetchOk::Done);
                }
                return Err(FetchErr::Retry(anyhow!("connection closed early")));
            }
            Err(e) => {
                ctx.commit(seg, &mut buf)?;
                return Err(FetchErr::Retry(e.into()));
            }
        }
    }
}

/// Fallback for servers without range support: one connection, start to end.
/// Can't resume, so a failure restarts from zero (a few times).
pub async fn download_single(
    client: Client,
    url: String,
    file: Arc<File>,
    job: Arc<Job>,
) -> anyhow::Result<()> {
    let mut attempt = 0;
    loop {
        match single_once(&client, &url, &file, &job).await {
            Ok(()) => return Ok(()),
            Err(_) if job.cancel.is_cancelled() => return Err(Cancelled.into()),
            Err(e) => {
                attempt += 1;
                if attempt >= 3 {
                    return Err(e);
                }
                tokio::select! {
                    _ = job.cancel.cancelled() => return Err(Cancelled.into()),
                    _ = sleep(Duration::from_secs(2 * attempt)) => {}
                }
            }
        }
    }
}

async fn single_once(client: &Client, url: &str, file: &File, job: &Job) -> anyhow::Result<()> {
    file.set_len(0)?;
    job.downloaded.store(0, Relaxed);
    job.workers.fetch_add(1, Relaxed);
    struct Count<'a>(&'a AtomicU32);
    impl Drop for Count<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Relaxed);
        }
    }
    let _count = Count(&job.workers);

    let req = client.get(url).header(header::ACCEPT_ENCODING, "identity");
    let mut resp = tokio::select! {
        _ = job.cancel.cancelled() => return Err(Cancelled.into()),
        r = req.send() => r?,
    };
    if !resp.status().is_success() {
        anyhow::bail!("server responded with {}", resp.status());
    }
    let expected = resp.content_length();
    let mut offset = 0u64;
    let mut buf: Vec<u8> = Vec::with_capacity(BUF_SIZE);
    let mut last_flush = Instant::now();
    let flush = |buf: &mut Vec<u8>, offset: &mut u64| -> anyhow::Result<()> {
        write_all_at(file, buf, *offset)?;
        *offset += buf.len() as u64;
        job.downloaded.store(*offset, Relaxed);
        buf.clear();
        Ok(())
    };
    loop {
        let next = tokio::select! {
            _ = job.cancel.cancelled() => return Err(Cancelled.into()),
            c = resp.chunk() => c?,
        };
        match next {
            Some(bytes) => {
                job.received.fetch_add(bytes.len() as u64, Relaxed);
                buf.extend_from_slice(&bytes);
                if !job.pace(bytes.len(), &job.cancel).await {
                    return Err(Cancelled.into());
                }
                if buf.len() >= BUF_SIZE || last_flush.elapsed() >= FLUSH_EVERY {
                    last_flush = Instant::now();
                    flush(&mut buf, &mut offset)?;
                }
            }
            None => {
                flush(&mut buf, &mut offset)?;
                break;
            }
        }
    }
    if let Some(len) = expected {
        if offset != len {
            anyhow::bail!("connection closed early ({offset} of {len} bytes)");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_smaller_near_the_end() {
        // 1.5 MiB left in one owned segment: too small to split normally,
        // but inside the tail zone, so an idle worker can take half.
        let job = Job::new();
        job.init(
            vec![Range {
                pos: 0,
                end: 3 * MIN_SPLIT / 2,
            }],
            Some(3 * MIN_SPLIT / 2),
        );
        let a = job.claim().unwrap();
        let b = job.claim().expect("tail split");
        assert_eq!(a.range().1, b.range().0);
        assert_eq!(b.range().1, 3 * MIN_SPLIT / 2);
        // Far from the end the normal minimum still applies.
        let big = Job::new();
        big.init(
            vec![
                Range {
                    pos: 0,
                    end: 3 * MIN_SPLIT / 2,
                },
                Range {
                    pos: 64 * MIN_SPLIT,
                    end: 128 * MIN_SPLIT,
                },
            ],
            Some(128 * MIN_SPLIT),
        );
        let _first = big.claim().unwrap();
        let _second = big.claim().unwrap();
        let third = big.claim().expect("splits the big segment");
        assert!(third.range().0 >= 64 * MIN_SPLIT);
    }

    #[test]
    fn splits_evenly() {
        let r = initial_ranges(10 * MIN_SPLIT + 3, 4);
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].pos, 0);
        assert_eq!(r[3].end, 10 * MIN_SPLIT + 3);
        for w in r.windows(2) {
            assert_eq!(w[0].end, w[1].pos);
        }
        assert_eq!(initial_ranges(100, 8).len(), 1);
        assert!(initial_ranges(0, 8).is_empty());
    }

    #[test]
    fn steals_back_half() {
        let job = Job::new();
        job.init(
            vec![Range {
                pos: 0,
                end: 8 * MIN_SPLIT,
            }],
            Some(8 * MIN_SPLIT),
        );
        let a = job.claim().unwrap();
        let b = job.claim().unwrap();
        assert_eq!(a.range(), (0, 4 * MIN_SPLIT));
        assert_eq!(b.range(), (4 * MIN_SPLIT, 8 * MIN_SPLIT));
        assert_eq!(job.remaining(), 8 * MIN_SPLIT);
    }
}
