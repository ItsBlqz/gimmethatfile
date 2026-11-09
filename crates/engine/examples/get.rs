//! Command-line download, handy for benchmarking the engine:
//!
//!     cargo run --release -p gtf-engine --example get -- <url> [connections] [dir]

use gtf_engine::{Manager, Settings, Status};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("usage: get <url> [connections] [dir]");
    let connections: u32 = args.next().map(|c| c.parse().unwrap()).unwrap_or(8);
    let dir = args
        .next()
        .map(Into::into)
        .unwrap_or_else(std::env::temp_dir);

    let state = tempfile_path();
    let m = Manager::load(&state)?;
    m.set_settings(Settings {
        download_dir: dir,
        max_concurrent: 1,
        connections,
        ..Default::default()
    });
    let id = m.add(&url, None, None)?;
    let start = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let s = m.snapshot().into_iter().find(|s| s.id == id).unwrap();
        let pct = s
            .total
            .map(|t| s.downloaded as f64 * 100.0 / t.max(1) as f64)
            .unwrap_or(0.0);
        eprint!(
            "\r{:>6.1}%  {:>8.2} MB/s  {:>2} conns  {:?}        ",
            pct,
            s.speed / 1e6,
            s.connections,
            s.status
        );
        match s.status {
            Status::Completed => {
                let secs = start.elapsed().as_secs_f64();
                eprintln!(
                    "\ndone: {} in {:.1}s, avg {:.2} MB/s -> {}/{}",
                    s.downloaded,
                    secs,
                    s.downloaded as f64 / secs / 1e6,
                    s.dir,
                    s.file_name.unwrap_or_default()
                );
                break;
            }
            Status::Failed => {
                eprintln!("\nfailed: {}", s.error.unwrap_or_default());
                break;
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_file(state);
    Ok(())
}

fn tempfile_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gtf-example-{}.json", std::process::id()))
}
