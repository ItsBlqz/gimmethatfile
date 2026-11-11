//! A small in-process HTTP file server that imitates different kinds of real
//! servers, shared by `examples/bench.rs` and `examples/perf.rs`.
//!
//! It runs on 127.0.0.1 inside the benchmark process; nothing leaves the
//! machine. What it can imitate:
//!
//! - `per_conn`: a speed cap per connection (most file hosts).
//! - `line`: a shared "line" all connections together can't exceed, with a
//!   congestion penalty: with `n` connections sending, it only delivers
//!   `rate / (1 + penalty * (n - 1))` (a home connection that is already full).
//! - `jitter`: each connection's speed jumps around every 100 ms between
//!   `1 - jitter` and `1 + jitter` times its cap (CDNs like kernel.org).
//! - `shared_jitter`: like `jitter`, but one factor for all connections at
//!   once (the whole server or path gets faster and slower together), which
//!   is what makes throughput measurements on real CDNs so noisy.
//! - `setup`: delay before the response starts (handshakes, redirects), which
//!   makes opening extra connections cost something.
//! - `max_conns`: more simultaneous requests than this get `429 Too Many
//!   Requests` (Hetzner, OVH).
//! - `ranges`: whether `Range` requests are honored.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Bytes/second per connection (`f64::INFINITY` = uncapped).
    pub per_conn: f64,
    /// Shared line: (bytes/second, congestion penalty).
    pub line: Option<(f64, f64)>,
    /// 0.0 = steady; 0.8 = each connection varies between 0.2x and 1.8x.
    pub jitter: f64,
    /// One speed factor shared by all connections, same range as `jitter`.
    pub shared_jitter: f64,
    /// How often the speed factors change.
    pub jitter_period: Duration,
    /// Delay before each response starts.
    pub setup: Duration,
    /// Answer 429 beyond this many simultaneous requests.
    pub max_conns: Option<usize>,
    /// Honor `Range` requests.
    pub ranges: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            per_conn: f64::INFINITY,
            line: None,
            jitter: 0.0,
            shared_jitter: 0.0,
            jitter_period: Duration::from_millis(100),
            setup: Duration::ZERO,
            max_conns: None,
            ranges: true,
        }
    }
}

struct Shared {
    cfg: ServerConfig,
    /// File size; the content is generated from the offset (see `byte_at`).
    len: u64,
    line: Option<Line>,
    /// Requests currently being handled (for `max_conns`).
    open: AtomicUsize,
    /// Seeds per-connection jitter so runs are repeatable.
    seed: AtomicU64,
    /// Shared jitter: (factor, since, rng).
    shared: Mutex<(f64, Instant, Rng)>,
}

impl Shared {
    fn shared_factor(&self) -> f64 {
        if self.cfg.shared_jitter <= 0.0 {
            return 1.0;
        }
        let mut g = self.shared.lock().unwrap();
        if g.1.elapsed() >= self.cfg.jitter_period {
            g.0 = 1.0 + self.cfg.shared_jitter * (2.0 * g.2.next_f64() - 1.0);
            g.1 = Instant::now();
        }
        g.0
    }
}

/// Start the server for a file of `len` bytes; it runs until the process
/// exits. The content is generated on the fly, so large files cost no memory.
pub async fn serve(len: u64, cfg: ServerConfig) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shared = Arc::new(Shared {
        line: cfg.line.map(|(rate, penalty)| Line {
            rate,
            penalty,
            active: AtomicUsize::new(0),
            next: Mutex::new(Instant::now()),
        }),
        cfg,
        len,
        open: AtomicUsize::new(0),
        seed: AtomicU64::new(0x9E37_79B9_7F4A_7C15),
        shared: Mutex::new((1.0, Instant::now(), Rng(0xD1B5_4A32_D192_ED03))),
    });
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let shared = shared.clone();
            tokio::spawn(async move {
                let _ = handle(sock, &shared).await;
            });
        }
    });
    addr
}

/// The file's content: deterministic, so downloads can be checked.
pub fn byte_at(offset: u64) -> u8 {
    (offset.wrapping_mul(31) % 251) as u8
}

/// A line shared by all connections (see the module docs).
struct Line {
    rate: f64,
    penalty: f64,
    active: AtomicUsize,
    /// When the line is free again: a virtual clock all connections share.
    next: Mutex<Instant>,
}

impl Line {
    /// Book `len` bytes on the line; returns when they have been "sent".
    fn reserve(&self, len: usize) -> Instant {
        let n = self.active.load(Ordering::Relaxed).max(1) as f64;
        let rate = self.rate / (1.0 + self.penalty * (n - 1.0));
        let mut next = self.next.lock().unwrap();
        // Let the clock lag a little behind real time: sleeps on Windows
        // overshoot by up to ~15 ms, and that time must not be lost.
        let now = Instant::now();
        let start = (*next).max(now.checked_sub(Duration::from_millis(30)).unwrap_or(now));
        *next = start + Duration::from_secs_f64(len as f64 / rate);
        *next
    }
}

/// xorshift64*: small, fast, deterministic.
struct Rng(u64);
impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Count<'a>(&'a AtomicUsize);
impl Drop for Count<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn handle(mut s: TcpStream, sh: &Shared) -> std::io::Result<()> {
    let mut req = Vec::new();
    let mut tmp = [0u8; 2048];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        req.extend_from_slice(&tmp[..n]);
    }

    let open = sh.open.fetch_add(1, Ordering::Relaxed) + 1;
    let _open = Count(&sh.open);
    if sh.cfg.max_conns.is_some_and(|m| open > m) {
        s.write_all(
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await?;
        return s.shutdown().await;
    }
    if !sh.cfg.setup.is_zero() {
        tokio::time::sleep(sh.cfg.setup).await;
    }

    let text = String::from_utf8_lossy(&req);
    let len = sh.len;
    let range = text
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("range")
                .then(|| v.trim().trim_start_matches("bytes=").to_string())
        })
        .filter(|_| sh.cfg.ranges);
    let (start, end) = match &range {
        Some(r) => {
            let (a, b) = r.split_once('-').unwrap();
            let b = if b.is_empty() {
                len - 1
            } else {
                b.parse().unwrap()
            };
            (a.parse::<u64>().unwrap(), (b + 1).min(len))
        }
        None => (0, len),
    };
    let head = match range {
        Some(_) => format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{len}\r\n",
            end - 1
        ),
        None => "HTTP/1.1 200 OK\r\n".into(),
    } + &format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        end - start
    );
    s.write_all(head.as_bytes()).await?;

    // Count this connection on the shared line while it sends.
    if let Some(l) = &sh.line {
        l.active.fetch_add(1, Ordering::Relaxed);
    }
    let _active = sh.line.as_ref().map(|l| Count(&l.active));

    let mut rng = Rng(sh.seed.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed) | 1);
    let mut mult = 1.0;
    let mut mult_since = Instant::now();
    // Pace by a virtual clock so the cap is accurate regardless of timer
    // resolution.
    let mut due = Instant::now();
    let mut buf = vec![0u8; 32 * 1024];
    let mut pos = start;
    while pos < end {
        let n = (end - pos).min(buf.len() as u64) as usize;
        for (i, b) in buf[..n].iter_mut().enumerate() {
            *b = byte_at(pos + i as u64);
        }
        let chunk = &buf[..n];
        pos += n as u64;
        s.write_all(chunk).await?;
        if sh.cfg.jitter > 0.0 && mult_since.elapsed() >= sh.cfg.jitter_period {
            mult = 1.0 + sh.cfg.jitter * (2.0 * rng.next_f64() - 1.0);
            mult_since = Instant::now();
        }
        let rate = sh.cfg.per_conn * mult * sh.shared_factor();
        due += Duration::from_secs_f64(chunk.len() as f64 / rate);
        let mut wake = due;
        if let Some(l) = &sh.line {
            wake = wake.max(l.reserve(chunk.len()));
        }
        if wake > Instant::now() {
            tokio::time::sleep_until(wake.into()).await;
        }
    }
    s.shutdown().await
}

/// Download `url` once with `conns` connections into a fresh folder under
/// `work`. Returns the time taken and the average number of open connections.
pub async fn download(work: &std::path::Path, url: &str, conns: u32) -> anyhow::Result<(f64, f64)> {
    use gtf_engine::{Manager, Settings, Status};
    let dir = work.join(format!("run-{}", SEQ.fetch_add(1, Ordering::Relaxed)));
    let m = Manager::load(dir.join("state.json"))?;
    m.set_settings(Settings {
        download_dir: dir.clone(),
        max_concurrent: 1,
        connections: conns,
        ..Default::default()
    });
    let start = Instant::now();
    let id = m.add(url, None, None)?;
    let (mut open, mut samples) = (0u64, 0u64);
    let result = loop {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let s = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        open += u64::from(s.connections);
        samples += 1;
        match s.status {
            Status::Completed => {
                break Ok((
                    start.elapsed().as_secs_f64(),
                    open as f64 / samples.max(1) as f64,
                ))
            }
            Status::Failed => break Err(anyhow::anyhow!("download failed: {:?}", s.error)),
            _ => {}
        }
    };
    m.shutdown();
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

static SEQ: AtomicU64 = AtomicU64::new(0);
