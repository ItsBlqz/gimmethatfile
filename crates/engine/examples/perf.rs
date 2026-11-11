//! Performance regression suite (used by CI, see CONTRIBUTING.md).
//!
//! Downloads from an in-process test server that imitates different kinds of
//! real servers (see `common/testserver.rs`) and prints one JSON line per run
//! to stdout, progress to stderr:
//!
//!     cargo run --release -p gtf-engine --example perf -- [--runs N] [--only a,b] [--list]
//!
//! `scripts/perf_compare.py` compares two result files (e.g. `main` vs a PR).
//! The numbers say whether a change makes the engine slower under these
//! conditions; they are not real-world speeds (see docs/BENCHMARKS.md).

#[path = "common/testserver.rs"]
mod testserver;

use std::time::Duration;
use testserver::ServerConfig;

const MB: f64 = 1e6;

struct Scenario {
    name: &'static str,
    what: &'static str,
    file_mib: usize,
    connections: u32,
    server: ServerConfig,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "capped",
            what: "each connection capped at 4 MB/s (most file hosts)",
            file_mib: 64,
            connections: 8,
            server: ServerConfig {
                per_conn: 4.0 * MB,
                ..Default::default()
            },
        },
        Scenario {
            name: "noisy",
            what: "1 MB/s per connection, speed swinging (per connection and all together) every 1.5 s, 50 ms setup; ~25 s (CDN like kernel.org)",
            file_mib: 192,
            connections: 8,
            server: ServerConfig {
                per_conn: 1.0 * MB,
                jitter: 0.8,
                shared_jitter: 0.6,
                jitter_period: Duration::from_millis(1500),
                setup: Duration::from_millis(50),
                ..Default::default()
            },
        },
        Scenario {
            name: "slow_setup",
            what: "4 MB/s per connection, 150 ms before each response (handshakes, redirects)",
            file_mib: 64,
            connections: 8,
            server: ServerConfig {
                per_conn: 4.0 * MB,
                setup: Duration::from_millis(150),
                ..Default::default()
            },
        },
        Scenario {
            name: "redirect",
            what: "4 MB/s per connection behind a link that takes 150 ms to redirect to the file",
            file_mib: 64,
            connections: 8,
            server: ServerConfig {
                per_conn: 4.0 * MB,
                redirect: Some(Duration::from_millis(150)),
                ..Default::default()
            },
        },
        Scenario {
            name: "throttled",
            what: "4 MB/s per connection, 429 beyond 3 connections",
            file_mib: 48,
            connections: 8,
            server: ServerConfig {
                per_conn: 4.0 * MB,
                max_conns: Some(3),
                ..Default::default()
            },
        },
        Scenario {
            name: "saturated",
            what: "20 MB/s line shared by all connections, congestion penalty 0.1",
            file_mib: 64,
            connections: 8,
            server: ServerConfig {
                line: Some((20.0 * MB, 0.1)),
                ..Default::default()
            },
        },
        Scenario {
            name: "no_ranges",
            what: "no range support (single connection), 8 MB/s",
            file_mib: 32,
            connections: 8,
            server: ServerConfig {
                per_conn: 8.0 * MB,
                ranges: false,
                ..Default::default()
            },
        },
    ]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut runs = 1usize;
    let mut only: Option<Vec<String>> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => runs = args.next().expect("--runs N").parse()?,
            "--only" => {
                only = Some(
                    args.next()
                        .expect("--only a,b")
                        .split(',')
                        .map(str::to_string)
                        .collect(),
                )
            }
            "--list" => {
                for s in scenarios() {
                    println!("{:<11} {}", s.name, s.what);
                }
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }

    let work = std::env::temp_dir().join(format!("gtf-perf-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    for s in scenarios()
        .into_iter()
        .filter(|s| only.as_ref().is_none_or(|o| o.iter().any(|n| n == s.name)))
    {
        let bytes = s.file_mib * 1024 * 1024;
        let addr = testserver::serve(bytes as u64, s.server).await;
        let url = format!("http://{addr}/{}.bin", s.name);
        for run in 0..runs {
            let (secs, conns) = testserver::download(&work, &url, s.connections).await?;
            let mbps = bytes as f64 / secs / MB;
            eprintln!(
                "{:<11} run {}: {mbps:6.2} MB/s  {secs:5.2} s  ~{conns:.1} conns",
                s.name,
                run + 1
            );
            println!(
                "{{\"scenario\":\"{}\",\"mbps\":{mbps:.4},\"secs\":{secs:.4},\"conns\":{conns:.2}}}",
                s.name
            );
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}
