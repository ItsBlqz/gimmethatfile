# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

First public release (will be tagged `v0.1.0`).

### Downloads

- Multi-connection downloads with dynamic segmentation (work stealing), so all
  connections stay busy until the end. Near the end of a download, idle
  connections split smaller pieces (down to 256 KiB) off slow ones.
- Connection reuse: a connection that finishes its segment continues into the
  next idle one instead of reconnecting.
- Connection count that adapts to the server: slow start (4 connections, then
  one more every 300 ms), backing off on `429`/`503` and recovering later, and
  as few connections as needed while a speed limit is the bottleneck.
  Reducing connections based on measured speed alone is experimental and off
  by default (`GTF_ADAPTIVE_DOWN=1`; see `docs/BENCHMARKS.md`).
- Pause and resume, which survive app restarts and crashes. A changed file
  (different size or ETag) is detected and restarted.
- Single-connection fallback for servers without range support.
- Download queue with a configurable number of simultaneous downloads.
- Speed limits: one for all downloads together (Settings → Speed) and one per
  download (in the add dialog under "More options", or later from the row's
  ⋯ menu). Both apply immediately to running downloads.
- Optional SHA-256 checksum in the add dialog, verified after the download
  finishes. A mismatch is flagged in red; the file is kept.
- Scheduler: only download during set hours on chosen days, including windows
  that cross midnight. Downloads outside the window show "Waiting for
  schedule", keep their progress and continue automatically; "Start now" runs
  a single download anyway.

### Browser integration

- Browser extension for Chrome and Edge (`extension/`, load unpacked for now)
  that sends browser downloads to the app, with a toolbar switch, connection
  status, filters (minimum size, file types, sites to skip), "let the next
  download through", and a **Download with Gimme That File** context-menu item.
  If the app isn't running or capture is off, the browser downloads the file
  itself.
- The app receives those downloads on `127.0.0.1:47811`, answering only
  browser extensions. **Settings → Browser** turns this off.
- Cookies, referrer and User-Agent from the browser are sent with every
  request of that download, so downloads behind a login work. They are kept in
  the local download list until the download completes (so resuming works)
  and then deleted.

### App

- Clean, flat UI with a custom title bar and System/Light/Dark themes: paste
  links anywhere, drag links onto the window, per-connection segment map,
  speed and time left.
- Tray icon: closing the window hides it and downloads keep running; quit from
  the tray menu. Launching the app again shows the running window.
- Settings: minimize to tray, start with Windows (hidden in the tray), and
  notifications when a download finishes, fails or fails its checksum.

### For contributors

- Performance checks in CI: every pull request is compared with `main` on six
  simulated server types and fails if it is slower by more than 3 standard
  errors and 5%, plus an informational run over an emulated long-distance
  network and a manual/monthly real-world benchmark (`scripts/perf_ab.sh`,
  `scripts/realworld.sh`, `examples/perf.rs`, `examples/bench.rs`).
- `GIMMETHATFILE_DATA_DIR` to use a separate download list, and `GTF_DEBUG=1`
  for engine logs.

[Unreleased]: https://github.com/ItsBlqz/gimmethatfile/commits/main
