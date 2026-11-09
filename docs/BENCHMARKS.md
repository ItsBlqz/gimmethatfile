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
| 8 | 10.0 s | 12.8 MB/s | 6.7x |
| 16 | 6.8 s | 19.0 MB/s | 9.9x |

The gap to a perfect 8x/16x is the slow-start ramp: the engine opens 4
connections, then adds one every 300 ms, so it doesn't trip servers' abuse
protection. Since the connection count became throughput-aware (see below),
the engine also checks once, about 2 s after reaching the maximum, whether
fewer connections would do as well. Here they don't, so it reverts, but the
check costs about 0.6 s with 8 connections (9.4 s before) and nothing
measurable with 16 or with 4 or fewer.

## 2. Reproducible: one connection already fills the line

The same benchmark can add a shared "line" that all connections together
can't exceed, with a congestion penalty: with `n` connections open the line
only delivers `line / (1 + penalty * (n - 1))`. This models what we measured
on a Debian mirror (section 4): one connection fills the line and more
connections make it slower.

```sh
cargo run --release -p gtf-engine --example bench -- 1000 256 20 0.1   # 20 MB/s line, penalty 0.1
```

"Avg. open" is the average number of open connections during the download.
"Before" is the engine with a fixed slow start (4, then +1 every 300 ms up to
the maximum) that it had until this change.

| Max. connections | Before | Avg. open | Now | Avg. open |
|---:|---:|---:|---:|---:|
| 1 | 18.9 MB/s | 1.0 | 18.9 MB/s | 1.0 |
| 2 | 17.2 MB/s | 2.0 | 18.6 MB/s | 1.1 |
| 4 | 14.6 MB/s | 3.9 | 18.0 MB/s | 1.5 |
| 8 | 11.5 MB/s | 7.6 | 17.9 MB/s | 1.7 |
| 16 | 8.2 MB/s | 14.6 | 17.9 MB/s | 1.7 |

A middle case, where each connection is capped at 4 MB/s *and* the line is
full at 20 MB/s (`-- 4 256 20 0.1`; the best here is 4 connections at
15.4 MB/s):

| Max. connections | Before | Avg. open | Now | Avg. open |
|---:|---:|---:|---:|---:|
| 1 | 3.8 MB/s | 1.0 | 3.8 MB/s | 1.0 |
| 8 | 11.0 MB/s | 7.6 | 13.7 MB/s | 4.0 |
| 16 | 7.9 MB/s | 14.6 | 10.9 MB/s | 3.1 |

The remaining gap to the best case is the time spent finding it: the climb
overshoots for a second or two, and every 20 s (backing off up to 160 s) a
probe tries one step up or down.

## 3. Engine overhead

The same benchmark with no cap (`-- 100000 1024`, a 1 GB file over loopback)
measures how fast the engine itself can go: **about 250–370 MB/s with 1
connection and 500–580 MB/s with 8** (2–5 Gbit/s) on a mid-range Windows
laptop with an NVMe SSD, depending on the run. Interleaved runs of the
engine before and after the speed-limit and connection-count changes were
indistinguishable within that noise. The engine is not the bottleneck on any
real internet connection.

## 4. Real world

These were measured on one home connection on Windows 11, in November 2026, with
`examples/get.rs`, with the engine before the throughput-aware connection
count. Every file's SHA-256 was checked: against the publisher's
checksum where one exists (Debian), and otherwise against a `curl` download of the
same file.

| Source | 1 connection | 8 connections | 16 connections | Notes |
|---|---:|---:|---:|---|
| kernel.org (Fastly CDN), 145 MB | 5.3–5.5 MB/s | 6.3–8.5 MB/s | 5.8 MB/s | Helps moderately |
| Hetzner speed test (Singapore), 100 MB | 3.8–5.6 MB/s | 5.3–6.3 MB/s | 5.5 MB/s | Allows ~2 connections per IP and answers the rest with `429`; the engine adapts |
| Debian mirror (Sweden, via redirect), 756 MB | 24.9–26.6 MB/s | 9.5–20.2 MB/s | 21.6 MB/s | One connection already fills the line; more connections only add congestion |

Readings vary by 2x or more between runs of the same test, so treat them as
indicative only.

### Debian mirror, with the throughput-aware connection count

Measured on 10 November 2026 on the same connection, with
`GTF_DEBUG=1 examples/get.rs`, runs interleaved, every file checked against
Debian's SHA-256. "Before" is the engine with the fixed slow start.

| Run | 1 connection | 8, before | 8, now |
|---|---:|---:|---:|
| 1 | 26.1 MB/s | 21.5 MB/s | 16.1 MB/s ¹ |
| 2 | ~22.5 MB/s ² | ~21 MB/s ² | ~24 MB/s ¹ ² |
| 3 | | | 19.7 MB/s |

¹ An earlier version of the controller, without the check after the climb.
² Interrupted near the end (the test script was stopped to rebuild); the
speed is the average up to that point.

**This is inconclusive.** On this day the line behaved differently from the
measurement above: 8 connections reached 32–37 MB/s for stretches, more than
one connection ever did, so the premise ("one connection fills the line") did
not hold. In run 3 the controller settled on 6 connections, tried 3 (−68%,
reverted) and 7 (−22%, reverted); the averages are dominated by stalls and
reconnects near the end of the downloads, not by the connection count. We
stopped at about 4.5 GB of traffic. The reproducible scenario in section 2 is
the better evidence that the controller does what it should; the real-world
numbers mainly show it doesn't break anything on a fast mirror.

### What this tells us

- **Servers that cap each connection** (most file hosts): large gains, close to
  linear in the number of connections.
- **Fast CDNs near you**: small gains, because your line is the limit.
- **When one connection already fills your line**, extra connections can *lower*
  throughput through congestion. The engine measures what added connections
  bring and backs off when they don't help (section 2); real networks are
  noisy enough that this works better in the reproducible setup than on a
  single real-world afternoon (section 4).
- **Servers with strict per-IP limits** (Hetzner, OVH): connection count must
  adapt, or the download stalls or the IP gets temporarily blocked. The engine
  handles this with slow start and backing off on `429`/`503`.

## Running your own

```sh
cargo build --release -p gtf-engine --example get
./target/release/examples/get <url> <connections> <output-dir>
# GTF_DEBUG=1 prints retries and connection-count changes
```

Contributions of measurements from other networks are welcome. Please open a
discussion or PR with your table and the commands you ran.
