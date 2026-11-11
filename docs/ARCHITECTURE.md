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
| `headers.rs` | Per-download request headers (cookies, referrer, ...): which are allowed, merging. |
| `job.rs` | One running download: segments, workers, work stealing, the supervisor. |
| `control.rs` | How many connections to use: a pure controller fed with throughput samples. |
| `limit.rs` | Speed limits: a token bucket shared by connections (global and per download). |
| `manager.rs` | List of downloads, queue/scheduler, pause/resume/remove, state file, speed ticker. |
| `fsutil.rs` | Positional writes (`seek_write`/`write_at`) and sparse files on NTFS. |
| `schedule.rs` | Scheduled hours: `Schedule::allows(weekday, minute)` is a pure function of local time. |

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
3. `Inner::drive` probes the server (retrying network errors and
   `429`/`5xx` twice) and decides between a **fresh** start and a **resume**.
   It resumes only if the size and ETag (or, without ETags, `Last-Modified`)
   are unchanged and the `.gtfpart` file exists at its full size. It then
   opens or creates the part file and runs either `download_ranged` (range
   support) or `download_single`.
4. On success, the part file is renamed to the final name. `Inner::run` records
   the outcome, saves, and calls `schedule()` again.

### Scheduled hours

`Settings.schedule` limits downloading to a daily window on chosen days. The
window is `[start, end)` in minutes after local midnight; `end < start` crosses
midnight and belongs to the day it starts on; `start == end` is the whole day.

- `State.schedule_open` caches `Schedule::allows_now()`. `apply_schedule()`
  recomputes it on every ticker tick (500 ms) and in `set_settings`.
- While closed, `schedule()` doesn't start `Queued` downloads, and
  `Snapshot.waiting_for_schedule` is set for them (the UI shows "Waiting for
  schedule").
- When the window closes, running downloads get status `Queued` *before* their
  job is stopped with `StopReason::Pause`. `run` keeps a `Queued` status after
  a pause-stop, so they are not shown as paused, keep their segments, and
  restart (resuming) when the window opens.
- `Manager::force_start` sets `Entry.forced` (not persisted), which ignores
  the schedule for that download until it is paused or the app restarts.

### Checksums and events

- With `expected_sha256` set, a successful download becomes `Completed` with
  `checksum: verifying`, frees its slot, and `Inner::verify` hashes the file on
  a blocking thread (`spawn_blocking`). The result (`ok`/`mismatch`) is
  persisted. A mismatch keeps the status `Completed`: the file is whole as the
  server sent it, so it's kept and the UI shows a warning instead of deleting
  or re-downloading it. Checks interrupted by quitting run again on load.
- `Manager::subscribe()` is a `broadcast` channel of `Event`s (`Completed`,
  `ChecksumMismatch`, `Failed`) sent from `run`/`verify`, i.e. only for
  downloads that finish while the app runs, never for pauses, removals or
  schedule stops. The app turns them into notifications (`app/src/notify.rs`).

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
  Every `206` must report the file size the probe saw; a different size
  means the file was replaced, and the download fails instead of mixing two
  versions. A lone connection that gets `429`/`503` waits for the server's
  `Retry-After` (at most 30 s).
- **redirects**: if the probe was redirected, segment requests go straight
  to the final address (`job::Source::resolved`), which saves a round trip
  per connection and keeps all connections on one mirror. A download with
  its own headers (cookies) only does this when the redirect stays on the
  same origin, so they are never sent to another host. When the final
  address answers `4xx` or can't be reached (expired signed link, mirror
  gone), the download goes back to the original link for the rest of the run.
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
congestion and *lower* the speed (see [BENCHMARKS.md](BENCHMARKS.md)).

**By default** the controller climbs to the configured maximum (slow start:
4, then +1 every 300 ms) and only goes lower for the two signals described
below: server push-back and a binding speed limit. Reducing connections
based on measured throughput alone (steps 1–3) is **experimental and off**
(`Controller::with_adaptive_down`, `GTF_ADAPTIVE_DOWN=1` for the CLI and
benchmarks). It works in the reproducible benchmark, but on a real CDN
(kernel.org, where each connection is individually slow) its 1–1.5 s
measurements were noisy enough that it kept removing connections that
helped, and downloads ran at less than half speed. With it switched on:

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
`GIMMETHATFILE_DATA_DIR`). It is written atomically (temporary file, flushed
to disk, then rename) on every state change and every 3 s while downloading. Downloads that
were running when the app quit are reloaded as `Queued` and continue on their own.

The partial file is `<name>.gtfpart`, pre-sized and marked sparse on Windows,
because NTFS would otherwise zero-fill everything before a far-away write.

## Browser integration

```
browser ──onCreated──▶ extension ──POST /v1/ping──▶ app (src/browser.rs, 127.0.0.1:47811)
   ▲   pause                │  ──POST /v1/downloads──▶ Manager::add_with(url, AddOptions{headers,..})
   └── cancel + erase ◀─────┘ (only after 201; otherwise resume / restart in the browser)
```

**Extension** (`extension/background.js`). On `downloads.onCreated` it skips
non-http(s) URLs, its own fallback downloads, private windows and whatever the
options filter out. It then **pauses** the browser download (so it doesn't
keep writing), pings the app (1.5 s timeout), and posts the URL, the browser's
suggested file name (`onDeterminingFilename` in Chrome), the referrer and the
cookies (`cookies.getAll`) for the URL. The browser's User-Agent is added only
when cookies are sent. Only after the app answers `201` does it cancel and
erase the browser download; on any other outcome it resumes it, or starts it
again if it can't be resumed. Invariant: **the browser download is cancelled
only after the app has accepted it.**

**Receiver** (`app/src/browser.rs`). `tiny_http` on its own thread, bound to
`127.0.0.1:47811`. If the port is taken it logs and stays off; the app works as
usual. All checks are in `route()` and `to_add_options()`, which are unit
tested: `Host` must be ours, `Origin` must be `chrome-extension://…` or
`moz-extension://…`, the add endpoint takes only JSON up to 64 KiB with an
http(s) URL. A request can't choose the folder: files go to the app's
download folder. The ping and add endpoints accept POST because Chrome omits
`Origin` on extension GET requests. With `Settings.browser_capture` off, ping
reports `capture: false` and adds answer `409`. On success the app emits a
`browser-added` event; the UI shows a toast without focusing the window.

**Engine.** `AddOptions.headers` go through `headers::sanitize` and are stored
in `DownloadRecord.headers` (persisted, so resume works). `Inner::drive`
builds a separate `reqwest::Client` with those headers as default headers for a
download that has any, so the probe, every ranged connection and the
single-connection fallback all send them, and `probe.rs`/`job.rs` stay unaware
of them. A provided `User-Agent` replaces the engine's. reqwest removes
`Cookie`/`Authorization` on redirects to another host. `GTF_DEBUG` logs header
names only.

## Testing

- Unit tests sit next to the code (`#[cfg(test)]`).
- `crates/engine/tests/e2e.rs` runs a tiny HTTP server in-process. It can switch
  range support on and off, slow responses down, limit connections with `429`,
  and answer `bytes=0-` with a plain `200`, like some CDNs. When you hit a
  server behavior in the wild, reproduce it there first.
- `crates/engine/tests/headers.rs` checks that per-download headers reach every
  request, using a server that answers `403` without the right cookie.
- `extension/test` (Playwright, `npm test`) loads the extension into Chromium
  and runs it against the real app: capture, fallbacks, and that web pages
  can't use the receiver.
