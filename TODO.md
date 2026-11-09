# TODO

Working list of what to build next. This file is the quick overview; bigger
items can also get an issue.

## Next

- [ ] **Slow end of downloads.** With several connections, the last ~2% can
      take several seconds when each connection to the server is slow (Debian
      mirror: 1 connection finishes its last 2% in ~1 s, 8 connections in
      4–9 s). Smaller splits near the end help in theory but didn't measurably
      change this. Ideas: race the slowest remaining segment on a second
      connection, or reuse the fastest connection.
- [ ] **Adaptive connection count on real servers.** It fixes the simulated
      "line already full" case (16 connections: 8.2 → 17.9 MB/s), but on the
      real Debian mirror results were inconclusive (noise of ±30% between runs).
      Needs more measurements across different servers, perhaps longer windows.
- [ ] **Clear stored cookies** from a download once it completes (they are kept
      in the local download list so resume works).
- [ ] **Dev instance next to the installed app**: let a debug build with
      `GIMMETHATFILE_DATA_DIR` set skip the single-instance check (and use
      another receiver port), so tests don't collide with a running copy.
- [ ] **Verify notifications on a machine with toasts enabled**, including
      clicking one to show the window.
- [ ] Global speed limit is shared per connection, not per download, so a
      download with more connections gets a bigger share.
- [ ] Test the extension in Firefox (manifest is prepared, untested).

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

- [x] Speed limits: global and per download (live, persisted, ⋯ row menu)
- [x] Throughput-aware connection count (adds connections only while they
      help; backs off on a saturated line; re-probes every 20 s)
- [x] Browser download capture: Chrome/Edge extension, local receiver on
      127.0.0.1:47811 that only answers extensions, per-download cookies/headers
- [x] Notifications when a download finishes or fails
- [x] Settings: minimize to tray, start with Windows (hidden in the tray)
- [x] Optional SHA-256 verification after download
- [x] Scheduler (set hours and days, overnight windows, "Start now" override)
- [x] Close button hides to the tray; downloads keep running; tray menu has
      Show / Pause all / Resume all / Quit
- [x] Single instance: launching again shows the running window
- [x] Rename to Gimme That File; flat UI with custom title bar and themes
- [x] Dependency updates: reqwest 0.13 (native TLS + system proxy),
      parking_lot 0.13, dirs 7, windows-sys 0.61; MSRV 1.95
