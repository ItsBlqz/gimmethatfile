# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Browser extension for Chrome and Edge (`extension/`, load unpacked for now)
  that sends browser downloads to the app, with a toolbar switch, connection
  status, filters (minimum size, file types, sites to skip), "let the next
  download through", and a **Download with Gimme That File** context-menu item.
  If the app isn't running or capture is off, the browser downloads the file
  itself.
- The app receives those downloads on `127.0.0.1:47811`, answering only
  browser extensions. **Settings → Browser** turns this off. A small
  notification shows what was added, without bringing the window forward.
- Per-download request headers: cookies, referrer and User-Agent from the
  browser are sent with every request of that download (probe and all
  connections), so downloads behind a login work. They are stored in the local
  download list so resuming works too.

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
