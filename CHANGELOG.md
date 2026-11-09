# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Speed limits: one for all downloads together (Settings → Speed) and one per
  download (in the add dialog under "More options", or later from the row's
  ⋯ menu). Both apply immediately to running downloads, also without range
  support, and the row shows when a download is limited.
- `examples/bench.rs` can simulate a shared line with congestion
  (`-- <per_conn> <file_MB> <line_MBps> <penalty>`), and `BENCH_CONNS=1,8`
  runs only some connection counts.

### Changed

- Throughput-aware connection count: the engine measures what extra
  connections bring and backs off when they don't help (for example when one
  connection already fills the line), re-checking every 20 s. While a speed
  limit is the bottleneck it uses as few connections as still reach it.
- A connection stops reading as soon as it reaches the end of its segment
  instead of buffering data that would be thrown away.

## [0.1.0] - 2026-11-08

First release.

### Added

- Multi-connection downloads with dynamic segmentation (work stealing), so all
  connections stay busy until the end.
- Connection reuse: a connection that finishes its segment continues into the
  next idle one instead of reconnecting.
- Adaptive connection count: slow start, backing off on `429`/`503`, and
  recovering later.
- Pause and resume, which survive app restarts and crashes. A changed file
  (different size or ETag) is detected and restarted.
- Download queue with a configurable number of simultaneous downloads.
- Single-connection fallback for servers without range support.
- Tray icon: closing the window hides it and downloads keep running; quit from
  the tray menu. Launching the app again shows the running window.
- Clean, flat UI with a custom title bar and System/Light/Dark themes: paste links anywhere, drag links onto the window,
  per-connection segment map, speed and time left.
- `GIMMETHATFILE_DATA_DIR` to use a separate download list, and `GTF_DEBUG=1`
  for engine logs.

[Unreleased]: https://github.com/ItsBlqz/gimmethatfile/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ItsBlqz/gimmethatfile/releases/tag/v0.1.0
