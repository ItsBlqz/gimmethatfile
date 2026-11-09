# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Notifications when a download finishes, fails, or fails its checksum
  (Settings → Behavior). Clicking one shows the window.
- Setting: the minimize button hides the window to the tray.
- Setting: start with Windows, hidden in the tray (`--hidden`).
- Optional SHA-256 checksum in the add dialog ("More options"), verified after
  the download finishes. A mismatch is flagged in red; the file is kept.
- Scheduler: only download during set hours on chosen days, including
  windows that cross midnight. Downloads outside the window show "Waiting for
  schedule", keep their progress, and continue automatically; "Start now"
  runs a single download anyway.

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
