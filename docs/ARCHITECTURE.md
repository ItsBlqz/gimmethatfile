# Architecture

This document is for contributors. It explains how a download flows through
the code and which invariants keep it correct.

```
┌──────────── app (Tauri) ────────────┐        ┌────────────── crates/engine ──────────────┐
│ ui/*.html,js,css  ── invoke ──▶ main.rs │ ─────▶ │ Manager ── spawns ──▶ Job (one per download) │
│        ▲                         │     │        │   │  queue, persistence     │ probe → workers │
│        └──── "downloads" event ◀─┘     │ ◀───── │   └─ snapshot() every 400 ms └─ file on disk   │
└─────────────────────────────────────┘        └───────────────────────────────────────────┘
```

The engine has no UI dependencies. The app is a thin layer: every Tauri command
maps to one `Manager` method, and a timer pushes `Manager::snapshot()` to the
web view.

## Modules (`crates/engine/src`)

| File | Responsibility |
|---|---|
| `types.rs` | Serializable types: `DownloadRecord` (persisted), `Snapshot` (sent to the UI), `Settings`. |
| `probe.rs` | Sends `Range: bytes=0-0` to learn size, range support, file name, ETag. |
| `naming.rs` | File name from `Content-Disposition` or the URL; sanitizing; unique names. |
| `job.rs` | One running download: segments, workers, work stealing, the supervisor. |
| `control.rs` | How many connections to use: a pure controller fed with throughput samples. |
| `limit.rs` | Speed limits: a token bucket shared by connections (global and per download). |
| `manager.rs` | List of downloads, queue/scheduler, pause/resume/remove, state file, speed ticker. |
| `fsutil.rs` | Positional writes (`seek_write`/`write_at`) and sparse files on NTFS. |

## Lifecycle of a download

```
Queued ──schedule()──▶ Connecting ──probe ok──▶ Downloading ──▶ Completed
   ▲                       │                        │
   └──── resume() ◀── Paused ◀──── pause() ──────────┤
                           Failed ◀──── error ───────┘
```

1. `Manager::add` stores a `DownloadRecord` with status `Queued`, saves it, and
   calls `schedule()`.
2. `schedule()` starts queued downloads while fewer than `max_concurrent` are
   running. Each running download has a `Job`.
3. `Inner::drive` probes the server and decides between a **fresh** start and a
   **resume**. It resumes only if the size and ETag are unchanged and the
   `.gtfpart` file exists. It then opens or creates the part file and runs either
   `download_ranged` (range support) or `download_single`.
4. On success, the part file is renamed to the final name. `Inner::run` records
   the outcome, saves, and calls `schedule()` again.

## Segmented downloading (`job.rs`)

The remaining bytes are a list of `Segment`s, each `[pos, end)` plus an `owned`
flag. Workers are tokio tasks, each holding one HTTP connection.

- **claim**: a worker takes an unowned segment, or splits the largest owned one
  in half and takes the back half (*work stealing*). Segments smaller than
  `2 × MIN_SPLIT` (2 MiB) aren't split.
- **fetch**: requests are open-ended (`bytes=pos-`). The worker buffers up to
  512 KiB or 500 ms of data, or until it reaches the segment's `end`, then
  **commits**: under the segment lock it writes `min(buffered, end - pos)`
  bytes at offset `pos` and advances `pos`.
- **extend**: when a segment finishes and the segment right after it is idle,
  the worker appends that segment to its own and keeps reading the same
  connection, with no new request.
- **supervisor** (`download_ranged`): every 150 ms it feeds a `Sample`
  (bytes received, open connections, time spent waiting for the speed
  limiter) to the `Controller` and keeps `target` workers alive. It opens at
  most one connection per 150 ms, so connections never arrive in a burst.
  When there are more workers than `target`, one of them **retires** after
  its next commit (`try_retire` takes it off the count atomically) and its
  segment becomes free for the others.
  - No bytes at all for 120 s: the download fails with the last error.

### Connection count (`control.rs`)

More connections help when the server or the path limits each connection.
When one connection already fills the line, extra connections only add
congestion and *lower* the speed (see [BENCHMARKS.md](BENCHMARKS.md)). The
controller therefore measures and keeps a change only if it paid off:

1. **Climb.** Start with `min(4, max)` connections and add one every 300 ms.
   Every second, compare the last second with the one before: the added
   connections must each bring at least 40% of what an average connection
   brought before. If not, go back to the earlier count and stop climbing.
2. **Descend.** After a climb stopped early, halve the count while that
   loses less than 7% of the speed. A climb that reached the maximum gets a
   step down 2 s later anyway: in the first seconds TCP itself is still
   ramping up, which makes every added connection look useful. (So does a
   download that started at its maximum of 4 or fewer.) Whenever a step down
   makes the download *faster*, the next step halves.
3. **Re-probe** every 20 s, alternating a step up and a step down (a quarter
   of the count, at least 1). A step up is kept if it gains at least
   `max(10%, 40% of the proportional gain)`, a step down if it loses less than
   7%; a kept step is followed by another in the same direction, and each
   rejected probe doubles the wait for that direction (up to 160 s).

A probe waits until its connections are actually open, lets them settle for
300 ms and measures for 1 s (an obvious gain is accepted after 0.5 s). Two
signals override the hill climb:

- **Server push-back:** a `429`/`503`/`403` (or a `200` for a mid-file range)
  while other workers are running: that worker retires, `target` drops to the
  number of workers that were fine, and the controller doesn't probe up for
  15 s, then only one connection at a time.
- **Speed limit:** when connections spend more than 20% of their time waiting
  for the limiter, the limit is the bottleneck, not the network. The
  controller then only probes down (halving first, every 5 s with the same
  backoff) and keeps the smallest count that still reaches the limit. When
  the limit stops binding, it climbs again.

The controller is pure (time and samples are inputs), so its unit tests run a
simulated network (per-connection caps, a congested line, a limit) without
real time passing.

### Speed limits (`limit.rs`)

A `RateLimiter` is a token bucket that works with *debt*: a connection
charges every chunk it receives, the balance may go negative, and the
connection sleeps until the debt is refilled. The long-run rate is exact
whatever the chunk size, and waiting happens in 50 ms slices that re-read the
rate, so a changed limit applies immediately. Unused allowance is kept for at
most 250 ms (no bursts after idle periods). The manager owns one global
limiter; each `Job` owns one for the download. A chunk is charged to both at
once and the connection waits for the longer of the two waits. Bytes that
are thrown away (past the end of a segment) are refunded. The global limit
is shared per connection, not per download, so a download with more
connections gets a larger share; the controller keeps that in check by using
few connections while a limit binds. The single-connection path
(`download_single`) is paced the same way.

### Invariants

- **Lock order:** the segment-list lock first, then one segment lock. `commit`
  takes only a segment lock. `claim` and `extend` hold the list lock and lock
  segments one at a time (`extend` holds two, but only while it also holds the
  list lock, so no one else can be holding two).
- **`end` only shrinks** while a segment is owned (by stealing), and `pos` only
  grows. Stealing picks `mid > pos`, so `pos ≤ end` always holds.
- **`pos` counts bytes that are on disk.** It is advanced only after
  `write_all_at` succeeds. Saved state therefore always *covers* every byte not
  yet written. After a crash, at worst some bytes are downloaded again; data is
  never lost or corrupted.
- Bytes received beyond a segment's `end` (because someone stole that part) are
  dropped (and refunded to the speed limiters).
- **Worker count:** `workers` is incremented when a worker starts and
  decremented exactly once: by its drop guard, or by `try_retire` (which then
  disarms the guard). `try_retire` only succeeds while `workers > target`, so
  retiring never closes the last connection.

## Persistence

`downloads.json` in the app data directory (override with
`GIMMETHATFILE_DATA_DIR`). It is written atomically (temporary file, then
rename) on every state change and every 3 s while downloading. Downloads that
were running when the app quit are reloaded as `Queued` and continue on their own.

The partial file is `<name>.gtfpart`, pre-sized and marked sparse on Windows,
because NTFS would otherwise zero-fill everything before a far-away write.

## Testing

- Unit tests sit next to the code (`#[cfg(test)]`).
- `crates/engine/tests/e2e.rs` runs a tiny HTTP server in-process. It can switch
  range support on and off, slow responses down, limit connections with `429`,
  and answer `bytes=0-` with a plain `200`, like some CDNs. When you hit a
  server behavior in the wild, reproduce it there first.
