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

#[path = "common/testserver.rs"]
mod testserver;

use testserver::ServerConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let per_conn_mbps: f64 = args.next().map(|a| a.parse().unwrap()).unwrap_or(2.0);
    let file_mb: usize = args.next().map(|a| a.parse().unwrap()).unwrap_or(64);
    let line_mbps: Option<f64> = args.next().map(|a| a.parse().unwrap());
    let penalty: f64 = args.next().map(|a| a.parse().unwrap()).unwrap_or(0.0);

    let cfg = ServerConfig {
        per_conn: per_conn_mbps * 1e6,
        line: line_mbps.map(|l| (l * 1e6, penalty)),
        ..Default::default()
    };
    let addr = testserver::serve((file_mb * 1024 * 1024) as u64, cfg).await;
    let url = format!("http://{addr}/bench.bin");
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
        let (secs, open) = testserver::download(&work, &url, conns).await?;
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
