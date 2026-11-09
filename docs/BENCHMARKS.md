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
cargo run --release -p gtf-engine --example bench -- 2 128   # 2 MB/s per connection, 128 MB file
```

| Connections | Time | Average speed | Speed-up |
|---:|---:|---:|---:|
| 1 | 67.1 s | 1.9 MB/s | 1.0x |
| 2 | 34.3 s | 3.7 MB/s | 2.0x |
| 4 | 17.7 s | 7.2 MB/s | 3.8x |
| 8 | 9.5 s | 13.5 MB/s | 7.1x |
| 16 | 6.6 s | 19.5 MB/s | 10.2x |

The gap to a perfect 8x/16x is the slow-start ramp: the engine opens 4
connections, then adds one every 300 ms, so it doesn't trip servers' abuse
protection. Since the connection count became throughput-aware (see below),
the engine also checks once, about 2 s after reaching the maximum, whether
fewer connections would do as well. Here they don't, so it reverts; the check
costs nothing measurable (8 connections: 9.5 s, 9.4 s before).

## 2. Reproducible: one connection already fills the line

The same benchmark can add a shared "line" that all connections together
can't exceed, with a congestion penalty: with `n` connections open the line
only delivers `line / (1 + penalty * (n - 1))`. This models what we see on the
Debian mirror (section 4): one connection fills the line and more connections
make it slower.

```sh
cargo run --release -p gtf-engine --example bench -- 1000 256 20 0.1   # 20 MB/s line, penalty 0.1
GTF_ADAPTIVE_DOWN=1 cargo run --release -p gtf-engine --example bench -- 1000 256 20 0.1
```

"Avg. open" is the average number of open connections. "Default" is what the
app does (slow start up to the maximum). "Experimental" turns on the
throughput-based reductions (`GTF_ADAPTIVE_DOWN=1`), which are **off by
default** because they misjudged a real CDN (section 4).

| Max. connections | Default | Avg. open | Experimental | Avg. open |
|---:|---:|---:|---:|---:|
| 1 | 18.9 MB/s | 1.0 | 18.9 MB/s | 1.0 |
| 8 | 11.5 MB/s | 7.6 | 17.9 MB/s | 1.7 |
| 16 | 8.2 MB/s | 14.8 | 17.9 MB/s | 1.7 |

In this model the experimental controller does exactly what it should. The
problem is real networks: their speed jumps around far more than this model,
and the controller can't yet tell that noise from a real effect. Until it can,
the practical advice for a server where one connection already fills your
line is to set **Connections per file** to 1 or 2 for that download.

## 3. Engine overhead (local loopback, not internet speed)

These numbers measure the engine itself, over a connection that never leaves
the machine; they are not download speeds you will see from the internet.

The same benchmark with no cap (`-- 100000 1024`, a 1 GB file over loopback)
measures how fast the engine itself can go: **about 250–370 MB/s with 1
connection and 500–580 MB/s with 8** (2–5 Gbit/s) on a mid-range Windows
laptop with an NVMe SSD, depending on the run. Interleaved runs of the
engine before and after the speed-limit and connection-count changes were
indistinguishable within that noise. The engine is not the bottleneck on any
real internet connection.

## 4. Real world

All on one home connection (Windows 11, roughly 200 Mbit/s), with
`examples/get.rs`. Runs are interleaved (rotating the order every round) so a
change in the network affects every variant alike, and every downloaded file
was checked against its known SHA-256 (Debian's published checksum, otherwise
a `curl` download of the same file). Speeds are averages over the whole
download, in MB/s. "Old" is the engine before the speed-limit and
connection-count work (fixed slow start); "now" is the current default.

### Clean run (10 November 2026)

Machine otherwise idle: no game running, game launchers open in the
background, about 0.2 MB/s of other traffic.

| Source | 1 connection | 8, now | 8, old |
|---|---:|---:|---:|
| kernel.org (Fastly CDN), 145 MB | 5.2, 4.0 | **6.85, 6.53, 6.70** | 6.00, 6.25, 6.54, 6.54, 6.12 |
| Debian mirror (Sweden, via redirect), 756 MB | **24.8, 27.9** | 18.7, 20.2 | 20.2, 21.0, 21.3, 21.9 |
| Hetzner speed test (Singapore), 100 MB | 5.4 | 5.6 | 5.6, 5.5 |

The experimental throughput-based controller (section 2), which was briefly
the default, measured on the same evening:

| Source | 8, experimental | Avg. open |
|---|---:|---:|
| kernel.org | 2.7, 2.4, 2.2, 2.7, 5.3 | 3.5–4.5 (7.3 in the 5.3 run) |
| Debian | 19.2, 19.7 | 7.5 |

On kernel.org each connection is individually slow, so 8 connections help
(+45% over one). The experimental controller "measured" that removing
connections barely hurt, removed them, and lost more than half the speed;
with the controller bypassed the same build ran at 6.5–6.7 MB/s. That is why
it is off by default.

### Real life: under load (9–10 November 2026)

The same kind of runs, measured while the machine was under heavy CPU load
and an online game (GTA Online) was using the network at the same time, which
is how a download manager often gets used.

| Source | 1 connection | 8, old | 8, experimental |
|---|---:|---:|---:|
| Debian mirror, 756 MB | 26.1, ~22.5 ¹ | 21.5, ~21 ¹ | 16.1, ~24 ¹, 19.7 |

¹ Interrupted near the end; the speed is the average up to that point.

Under load the numbers spread more (16–24 MB/s for the same configuration),
but the picture is the same as on the idle machine: on this mirror one
connection is fastest, and the bottleneck is the line and the mirror, not the
PC. Within single runs, 8 connections reached 32–37 MB/s for stretches, so
which configuration wins also depends on the moment.

### First measurements (8 November 2026, first engine version)

| Source | 1 connection | 8 connections | 16 connections |
|---|---:|---:|---:|
| kernel.org, 145 MB | 5.3–5.5 | 6.3–8.5 | 5.8 |
| Hetzner, 100 MB | 3.8–5.6 | 5.3–6.3 | 5.5 |
| Debian mirror, 756 MB | 24.9–26.6 | 9.5–20.2 | 21.6 |

### What this tells us

- **Servers that cap each connection** (most file hosts): large gains, close
  to linear in the number of connections (section 1).
- **CDNs near you** (kernel.org): moderate gains, about +45% with 8
  connections over one.
- **When one connection already fills your line** (the Debian mirror here):
  one connection is fastest, about 25% ahead of 8. The engine can't detect
  this reliably yet; set the download to 1–2 connections for such servers.
- **Strict per-IP limits** (Hetzner, OVH): the engine backs off on
  `429`/`503` and ends up at the ~2 connections the server allows, at the same
  speed as a single connection.
- **Open question:** on the Debian mirror the current engine is about 10%
  slower than the old one with the same number of connections (4 of 4 pairs).
  Every connection there goes through a redirect, and the current engine opens
  more connections near the end of a download; that is the main suspect.

## Running your own

```sh
cargo build --release -p gtf-engine --example get
./target/release/examples/get <url> <connections> <output-dir>
# GTF_DEBUG=1 prints retries and connection-count changes
# GTF_ADAPTIVE_DOWN=1 turns on the experimental throughput-based controller
```

Contributions of measurements from other networks are welcome. Please open a
discussion or PR with your table and the commands you ran.
