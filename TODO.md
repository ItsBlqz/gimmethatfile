# TODO

Working list of what to build next. This file is the quick overview; bigger
items can also get an issue.

## In progress

- [ ] **Throughput-aware connection count.** Only add connections while total
      speed actually improves, and drop back when it falls. Today, extra
      connections can be slower than one when a single connection already fills
      the line (Debian mirror: 26 MB/s with 1 connection vs 9–20 MB/s with 8;
      see `docs/BENCHMARKS.md`).
- [ ] **Speed limits**: a global limit, plus an optional limit per download.
- [ ] **Browser download capture** (see below), including per-download request
      headers and cookies in the engine.
- [ ] **Notification** when a download finishes or fails.
- [ ] **Setting: minimize to tray** (today only the close button does that).
- [ ] **Start with Windows** (optional; starts hidden in the tray).
- [ ] **SHA-256 verification**: optionally paste the expected hash when adding a
      download and check it after completion.
- [ ] **Scheduler**: only download between set times.

### Browser download capture
Catch downloads in Chrome/Edge/Firefox and hand them to the app instead of the
browser's own downloader.

- A WebExtension (Manifest V3) that sees new browser downloads, cancels them,
  and sends the URL, file name, referrer and any cookies the server needs to
  the app.
- The app accepts these over a localhost endpoint that only answers browser
  extensions, not web pages.
- If the app isn't running or capture is off, the browser downloads the file
  as usual.
- Settings: file types and minimum size to capture, sites to skip, and a way to
  let one download through to the browser.

## Backlog

- [ ] Auto-update (`tauri-plugin-updater` + signed update manifest). The update
      feed is served from GitHub Releases, which needs the repo to be public.
- [ ] Code signing for the Windows installer (e.g. SignPath, free for open
      source projects, which means the repo has to be public).
- [ ] Code of Conduct contact email in `CODE_OF_CONDUCT.md`.
- [ ] macOS and Linux release builds.
- [ ] Publish: make the repo public, run `scripts/protect-main.sh`, enable
      private vulnerability reporting, tag `v0.1.0`.

## Done

- [x] Close button hides to the tray; downloads keep running; tray menu has
      Show / Pause all / Resume all / Quit
- [x] Single instance: launching again shows the running window
- [x] Rename to Gimme That File; flat UI with custom title bar and themes
- [x] Dependency updates: reqwest 0.13 (native TLS + system proxy),
      parking_lot 0.13, dirs 7, windows-sys 0.61; MSRV 1.95
