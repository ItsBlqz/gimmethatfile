# Benchmarks

Download speed depends on three things: the server, the path between you and it,
and your own connection. Multiple connections help when **the server or the path
limits each connection**, which is common. They can't make your line faster than
it is.

There are two kinds of numbers below: a reproducible local benchmark that anyone
can run, and real-world measurements, which you should treat as examples.

## 1. Reproducible: per-connection speed cap

`examples/bench.rs` starts a local HTTP server that caps every connection at a
fixed speed, as many real file hosts do, then downloads the same file with
1, 2, 4, 8 and 16 connections. No internet is needed.

```sh
cargo run --release -p vdl-engine --example bench -- 2 128   # 2 MB/s per connection, 128 MB file
```

| Connections | Time | Average speed | Speed-up |
|---:|---:|---:|---:|
| 1 | 67.2 s | 1.9 MB/s | 1.0x |
| 2 | 33.9 s | 3.8 MB/s | 2.0x |
| 4 | 17.5 s | 7.3 MB/s | 3.8x |
| 8 | 9.4 s | 13.6 MB/s | 7.1x |
| 16 | 6.7 s | 19.0 MB/s | 10.0x |

The gap to a perfect 8x/16x is the slow-start ramp: the engine opens 4
connections, then adds one every 300 ms, so it doesn't trip servers' abuse
protection.

## 2. Engine overhead

The same benchmark with no cap (`-- 100000 1024`, a 1 GB file over loopback)
measures how fast the engine itself can go: **370 MB/s with 1 connection and about
580 MB/s with 8** (3–5 Gbit/s) on a mid-range Windows laptop with an NVMe SSD. The
engine is not the bottleneck on any real internet connection.

## 3. Real world

These were measured on one home connection on Windows 11, in November 2026, with
`examples/get.rs`. Every file's SHA-256 was checked: against the publisher's
checksum where one exists (Debian), and otherwise against a `curl` download of the
same file.

| Source | 1 connection | 8 connections | 16 connections | Notes |
|---|---:|---:|---:|---|
| kernel.org (Fastly CDN), 145 MB | 5.3–5.5 MB/s | 6.3–8.5 MB/s | 5.8 MB/s | Helps moderately |
| Hetzner speed test (Singapore), 100 MB | 3.8–5.6 MB/s | 5.3–6.3 MB/s | 5.5 MB/s | Allows ~2 connections per IP and answers the rest with `429`; the engine adapts |
| Debian mirror (Sweden, via redirect), 756 MB | 24.9–26.6 MB/s | 9.5–20.2 MB/s | 21.6 MB/s | One connection already fills the line; more connections only add congestion |

Readings vary by 2x or more between runs of the same test, so treat them as
indicative only.

### What this tells us

- **Servers that cap each connection** (most file hosts): large gains, close to
  linear in the number of connections.
- **Fast CDNs near you**: small gains, because your line is the limit.
- **When one connection already fills your line**, extra connections can *lower*
  throughput through congestion. The engine doesn't detect this yet; see the
  "throughput-aware connection count" item in the roadmap.
- **Servers with strict per-IP limits** (Hetzner, OVH): connection count must
  adapt, or the download stalls or the IP gets temporarily blocked. The engine
  handles this with slow start and backing off on `429`/`503`.

## Running your own

```sh
cargo build --release -p vdl-engine --example get
./target/release/examples/get <url> <connections> <output-dir>
# VDL_DEBUG=1 prints retries and connection-count changes
```

Contributions of measurements from other networks are welcome. Please open a
discussion or PR with your table and the commands you ran.
