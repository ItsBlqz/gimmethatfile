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
| `job.rs` | One running download: segments, workers, work stealing, adaptive connections. |
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
  512 KiB or 500 ms of data, then **commits**: under the segment lock it writes
  `min(buffered, end - pos)` bytes at offset `pos` and advances `pos`.
- **extend**: when a segment finishes and the segment right after it is idle,
  the worker appends that segment to its own and keeps reading the same
  connection, with no new request.
- **supervisor** (`download_ranged`): keeps `target` workers alive.
  - Slow start: 4 connections, then +1 every 300 ms up to the configured maximum.
  - A `429`/`503`/`403` (or a `200` for a mid-file range) while other workers
    are running: that worker retires and `target` drops by one.
  - After 15 s without throttling, `target` goes up by one again.
  - No bytes at all for 120 s: the download fails with the last error.

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
  dropped.

## Persistence

`downloads.json` in the app data directory (override with
`GIMMETHATFILE_DATA_DIR`). It is written atomically (temporary file, then
rename) on every state change and every 3 s while downloading. Downloads that
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
http(s) URL. The ping and add endpoints accept POST because Chrome omits
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
