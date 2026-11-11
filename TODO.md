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
- [ ] **Throughput-based connection count (experimental, off by default).**
      It fixes the simulated "line already full" case (16 connections: 8.2 →
      17.9 MB/s), but on kernel.org it misread noise and cut speed by more
      than half, and on the Debian mirror it never backed off. Needs a more
      robust signal (longer or repeated measurements, confidence intervals)
      before it can be on by default. Try with `GTF_ADAPTIVE_DOWN=1`.
- [ ] **~10% slower than the old engine on the Debian mirror** with the same
      number of connections (4 of 4 interleaved pairs). Suspects: more
      connections opened near the end (smaller tail splits), each going
      through Debian's redirect. The redirect is now resolved once and
      segment requests use the final URL: +12% in the local `redirect`
      benchmark, but on the Debian mirror itself only +3% over 4 interleaved
      pairs (19.7 → 20.3 MB/s), which is within the noise. The redirect
      wasn't the main cause.
- [ ] **Flaky speed-limit tests.** `per_download_limit_is_accurate` (and its
      siblings `global_speed_limit_is_accurate` and
      `per_download_limit_works_without_ranges` in `tests/e2e.rs`) time the
      whole download, including probe, connection setup and the final
      flush/rename, and require 0.88–1.08× the limit. On a busy machine that
      overhead pushes the result just under 0.88 (seen: 0.87 with the full
      suite running in parallel). Loosen the lower bound in `assert_rate` to
      about 0.80 (keep the upper bound strict), or time only first byte to
      last byte.
- [ ] **Dev instance next to the installed app**: let a debug build with
      `GIMMETHATFILE_DATA_DIR` set skip the single-instance check (and use
      another receiver port), so tests don't collide with a running copy.
- [ ] **Verify notifications on a machine with toasts enabled**, including
      clicking one to show the window.
- [ ] Global speed limit is shared per connection, not per download, so a
      download with more connections gets a bigger share.
- [ ] Test the extension in Firefox (manifest is prepared, untested).

## Backlog

- [ ] Auto-update (`tauri-plugin-updater` + signed update manifest), with the
      update feed served from GitHub Releases.
- [ ] Code signing for the Windows installer (SignPath Foundation, free for
      open source projects), so SmartScreen stops warning.
- [ ] macOS and Linux release builds.

## Done

- [x] Speed limits: global and per download (live, persisted, ⋯ row menu)
- [x] Connection controller: slow start, backing off on 429/503, and few
      connections while a speed limit binds (throughput-based reductions are
      experimental, see Next)
- [x] Captured cookies are deleted when a download completes
- [x] Pre-release cleanup: changelog folded into the first release, new
      screenshots, cargo-deny (licences/advisories) in CI, history checked for
      secrets, social preview image (docs/social-preview.png)
- [x] Code of Conduct reporting via GitHub's Report content / private reports
      (no email)
- [x] Final benchmark on an idle machine plus an "under load" real-life run
      (docs/BENCHMARKS.md)
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
