# TODO

Working list of what to build next. Bigger items get an issue once the repo is
public; this file is the quick overview.

## Before going public

- [ ] **Throughput-aware connection count.** Only add connections while total
      speed actually improves, and drop back when it falls. Today, extra
      connections can be slower than one when a single connection already fills
      the line (Debian mirror: 26 MB/s with 1 connection vs 9–20 MB/s with 8;
      see `docs/BENCHMARKS.md`).
- [ ] **Code of Conduct contact.** Replace `[contact email to be added]` in
      `CODE_OF_CONDUCT.md`.
- [ ] **Triage Dependabot PRs** after the rename: merge the GitHub Actions bumps;
      test `reqwest` 0.13 on real HTTPS downloads (its TLS backend changed);
      check why `parking_lot` 0.13 fails CI.
- [ ] **Browser download capture** (see below).
- [ ] Make the repo public, then run `scripts/protect-main.sh` to turn on branch
      protection, and enable private vulnerability reporting
      (Settings → Code security).
- [ ] Tag `v0.1.0` and publish the draft release.

## Features

### Browser download capture
Catch downloads in Chrome/Edge/Firefox and hand them to the app instead of the
browser's own downloader.

- A WebExtension (Manifest V3) that listens to `chrome.downloads.onCreated`,
  cancels the browser's download and sends the URL, plus any cookies, referrer
  and user agent the server needs, to the app.
- Transport options: **native messaging** (most robust; the installer registers
  a host manifest) or a **localhost HTTP endpoint** with a per-install token
  (simpler, works for every browser).
- Include cookies/headers so downloads that need a logged-in session work. The
  engine needs per-download request headers for this.
- Settings: file types and minimum size to capture, sites to skip, and a
  "hold Alt to let the browser download" escape hatch.
- Use the single-instance hook (already in place) to receive URLs when the app
  is launched with arguments.

### Other
- [ ] Global and per-download speed limits
- [ ] Notification when a download finishes (`tauri-plugin-notification`)
- [ ] Setting: minimize button also goes to the tray (today only × does)
- [ ] Start with Windows (optional, starts in the tray)
- [ ] Auto-update (`tauri-plugin-updater` + signed update manifest)
- [ ] Code signing for the Windows installer (e.g. SignPath, free for open source)
- [ ] Optional SHA-256 verification after download
- [ ] Scheduler (start/stop at set times)
- [ ] Per-download request headers and cookies (also needed for browser capture)
- [ ] macOS and Linux release builds

## Done

- [x] Close button hides to the tray; downloads keep running; tray menu has
      Show / Pause all / Resume all / Quit
- [x] Single instance: launching again shows the running window
- [x] Rename to Gimme That File; flat UI with custom title bar and themes
