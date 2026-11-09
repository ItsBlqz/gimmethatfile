//! Reproducible benchmark: no internet needed.
//!
//! Starts a local HTTP server that caps every connection at a fixed speed
//! (many real hosts do this) and downloads the same file with 1, 2, 4, 8 and
//! 16 connections.
//!
//!     cargo run --release -p gtf-engine --example bench -- [per_conn_MBps] [file_MB]
//!
//! Defaults: 2 MB/s per connection, 64 MB file.

use gtf_engine::{Manager, Settings, Status};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let per_conn_mbps: f64 = args.next().map(|a| a.parse().unwrap()).unwrap_or(2.0);
    let file_mb: usize = args.next().map(|a| a.parse().unwrap()).unwrap_or(64);

    let body: Arc<Vec<u8>> = Arc::new(
        (0..file_mb * 1024 * 1024)
            .map(|i| (i * 31 % 251) as u8)
            .collect(),
    );
    let addr = serve(body.clone(), per_conn_mbps * 1e6).await;
    let work = std::env::temp_dir().join(format!("gtf-bench-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;

    println!("Local server: {file_mb} MB file, each connection capped at {per_conn_mbps} MB/s\n");
    println!("| Connections | Time | Average speed | Speed-up |");
    println!("|---:|---:|---:|---:|");
    let mut baseline = None;
    for conns in [1u32, 2, 4, 8, 16] {
        let secs = run(&work, addr, conns).await?;
        let speed = file_mb as f64 / secs;
        let base = *baseline.get_or_insert(secs);
        println!(
            "| {conns} | {secs:.1} s | {speed:.1} MB/s | {:.1}x |",
            base / secs
        );
    }
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

async fn run(work: &std::path::Path, addr: SocketAddr, conns: u32) -> anyhow::Result<f64> {
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
    loop {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let s = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        match s.status {
            Status::Completed => break,
            Status::Failed => anyhow::bail!("download failed: {:?}", s.error),
            _ => {}
        }
    }
    Ok(start.elapsed().as_secs_f64())
}

async fn serve(body: Arc<Vec<u8>>, bytes_per_sec: f64) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let body = body.clone();
            tokio::spawn(async move {
                let _ = handle(sock, body, bytes_per_sec).await;
            });
        }
    });
    addr
}

async fn handle(mut s: TcpStream, body: Arc<Vec<u8>>, rate: f64) -> std::io::Result<()> {
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

    // Pace by wall clock so the cap is accurate regardless of timer resolution.
    let t0 = Instant::now();
    let mut sent = 0usize;
    for chunk in body[start as usize..end as usize].chunks(32 * 1024) {
        s.write_all(chunk).await?;
        sent += chunk.len();
        let due = Duration::from_secs_f64(sent as f64 / rate);
        if let Some(wait) = due.checked_sub(t0.elapsed()) {
            tokio::time::sleep(wait).await;
        }
    }
    s.shutdown().await
}
