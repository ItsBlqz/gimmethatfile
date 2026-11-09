//! Reproducible benchmark: no internet needed.
//!
//! Starts a local HTTP server that caps every connection at a fixed speed
//! (many real hosts do this) and downloads the same file with 1, 2, 4, 8 and
//! 16 connections.
//!
//!     cargo run --release -p gtf-engine --example bench -- [per_conn_MBps] [file_MB] [line_MBps] [penalty]
//!
//! Defaults: 2 MB/s per connection, 64 MB file, no line limit. Set
//! `BENCH_CONNS=1,8` to run only some connection counts.
//!
//! `line_MBps` adds a shared "line" that all connections together can't
//! exceed, like a home connection. `penalty` models congestion: with `n`
//! connections open, the line only delivers `line / (1 + penalty * (n - 1))`.
//! For example, `-- 1000 256 20 0.1` is a server that one connection can
//! already max out, on a 20 MB/s line where 8 open connections get 11.8 MB/s.

use gtf_engine::{Manager, Settings, Status};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let per_conn_mbps: f64 = args.next().map(|a| a.parse().unwrap()).unwrap_or(2.0);
    let file_mb: usize = args.next().map(|a| a.parse().unwrap()).unwrap_or(64);
    let line_mbps: Option<f64> = args.next().map(|a| a.parse().unwrap());
    let penalty: f64 = args.next().map(|a| a.parse().unwrap()).unwrap_or(0.0);

    let body: Arc<Vec<u8>> = Arc::new(
        (0..file_mb * 1024 * 1024)
            .map(|i| (i * 31 % 251) as u8)
            .collect(),
    );
    let line = line_mbps.map(|l| {
        Arc::new(Line {
            rate: l * 1e6,
            penalty,
            active: AtomicUsize::new(0),
            next: Mutex::new(Instant::now()),
        })
    });
    let addr = serve(body.clone(), per_conn_mbps * 1e6, line).await;
    let work = std::env::temp_dir().join(format!("gtf-bench-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;

    print!("Local server: {file_mb} MB file, each connection capped at {per_conn_mbps} MB/s");
    if let Some(l) = line_mbps {
        print!(", shared line {l} MB/s, congestion penalty {penalty}");
    }
    println!("\n");
    println!("| Connections | Time | Average speed | Speed-up | Avg. open |");
    println!("|---:|---:|---:|---:|---:|");
    // BENCH_CONNS=1,8 runs only those connection counts.
    let counts: Vec<u32> = std::env::var("BENCH_CONNS")
        .map(|v| v.split(',').map(|c| c.trim().parse().unwrap()).collect())
        .unwrap_or_else(|_| vec![1, 2, 4, 8, 16]);
    let mut baseline = None;
    for conns in counts {
        let (secs, open) = run(&work, addr, conns).await?;
        let speed = file_mb as f64 / secs;
        let base = *baseline.get_or_insert(secs);
        println!(
            "| {conns} | {secs:.1} s | {speed:.1} MB/s | {:.1}x | {open:.1} |",
            base / secs
        );
    }
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

/// Returns the time taken and the average number of open connections.
async fn run(work: &std::path::Path, addr: SocketAddr, conns: u32) -> anyhow::Result<(f64, f64)> {
    let dir = work.join(format!("c{conns}"));
    let m = Manager::load(dir.join("state.json"))?;
    m.set_settings(Settings {
        download_dir: dir.clone(),
        max_concurrent: 1,
        connections: conns,
        ..Default::default()
    });
    let start = Instant::now();
    let id = m.add(&format!("http://{addr}/bench.bin"), None, None)?;
    let (mut open, mut samples) = (0u64, 0u64);
    loop {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let s = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        open += u64::from(s.connections);
        samples += 1;
        match s.status {
            Status::Completed => break,
            Status::Failed => anyhow::bail!("download failed: {:?}", s.error),
            _ => {}
        }
    }
    Ok((
        start.elapsed().as_secs_f64(),
        open as f64 / samples.max(1) as f64,
    ))
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

async fn serve(body: Arc<Vec<u8>>, bytes_per_sec: f64, line: Option<Arc<Line>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let (body, line) = (body.clone(), line.clone());
            tokio::spawn(async move {
                let _ = handle(sock, body, bytes_per_sec, line).await;
            });
        }
    });
    addr
}

async fn handle(
    mut s: TcpStream,
    body: Arc<Vec<u8>>,
    rate: f64,
    line: Option<Arc<Line>>,
) -> std::io::Result<()> {
    let mut req = Vec::new();
    let mut tmp = [0u8; 2048];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        req.extend_from_slice(&tmp[..n]);
    }
    let text = String::from_utf8_lossy(&req);
    let len = body.len() as u64;
    let range = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("range")
            .then(|| v.trim().trim_start_matches("bytes=").to_string())
    });
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
    struct Active<'a>(Option<&'a Line>);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            if let Some(l) = self.0 {
                l.active.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
    if let Some(l) = &line {
        l.active.fetch_add(1, Ordering::Relaxed);
    }
    let _active = Active(line.as_deref());

    // Pace by wall clock so the cap is accurate regardless of timer resolution.
    let t0 = Instant::now();
    let mut sent = 0usize;
    for chunk in body[start as usize..end as usize].chunks(32 * 1024) {
        s.write_all(chunk).await?;
        sent += chunk.len();
        let mut due = t0 + Duration::from_secs_f64(sent as f64 / rate);
        if let Some(l) = &line {
            due = due.max(l.reserve(chunk.len()));
        }
        if due > Instant::now() {
            tokio::time::sleep_until(due.into()).await;
        }
    }
    s.shutdown().await
}
