# Vibe Downloader

A fast, clean download manager written in Rust. Paste a link, pick a folder, done.

- **Multi-connection downloads**: splits a file into segments and downloads them in parallel.
- **Dynamic segmentation**: a connection that finishes early takes over half of the largest remaining segment, so all connections stay busy until the end.
- **Adaptive connection count**: starts with 4 connections and adds more while the server is happy. It backs off on `429`/`503` responses and retries later, like TCP congestion control.
- **Pause / resume**, including after closing the app or a crash. Progress is saved every few seconds.
- **Queue**: you choose how many downloads run at once.
- **Safe**: downloads go to `name.ext.vdpart` and are renamed only when complete. Existing files are never overwritten.
- Paste links anywhere (`Ctrl+V`), drag links onto the window, `Ctrl+N` to add. Light and dark themes follow the system.

## How it works (the short version)

Most servers limit how fast a *single* connection can go, and a single TCP connection over a long distance can't fill a fast line. HTTP lets a client ask for part of a file:

```
GET /file.iso
Range: bytes=52428800-
```

The server replies `206 Partial Content` with just that slice. So we:

1. **Probe**: request `bytes=0-0`. A `206` reply means ranges work, and its `Content-Range: bytes 0-0/1234567` header gives the total size. Otherwise we fall back to a single connection.
2. **Split** the file into N ranges and open one connection per range. Each connection writes its bytes straight into the right offset of a pre-sized file.
3. **Steal work**: when a connection finishes, it splits the biggest remaining range in half and takes the back half. If the range right after it is idle, it simply keeps reading on the same connection (requests are open-ended), which avoids a reconnect.
4. **Adapt**: if the server says "too many requests", drop a connection; when things are calm, try one more.
5. **Persist** the list of missing ranges, so resume picks up exactly where it stopped. On resume, the server's `ETag` and size are checked so we never stitch together two different versions of a file.

## Project layout

```
crates/engine/      Download engine: no UI, fully testable
  src/probe.rs        Range support, size, filename detection
  src/job.rs          Segmented download, work stealing, adaptive connections
  src/manager.rs      Queue, pause/resume, persistence, speed meter
  tests/e2e.rs        End-to-end tests against a local HTTP server
  examples/get.rs     CLI for benchmarking
app/                Tauri v2 desktop shell (Rust commands to UI)
ui/                 Plain HTML/CSS/JS front end (no build step)
```

## Running

Requirements: [Rust](https://rustup.rs) (stable). On Windows, the MSVC build tools and WebView2, which Windows 11 already has.

```sh
# Run the app (debug)
cargo run -p vibe-downloader

# Build an installer (outputs to target/release/bundle/nsis/)
cargo install tauri-cli --version "^2" --locked   # once
cd app && cargo tauri build

# Engine tests
cargo test -p vdl-engine

# Benchmark from the command line: url, connections, output dir
cargo run --release -p vdl-engine --example get -- https://example.com/big.bin 8 ./out
# Set VDL_DEBUG=1 to log retries and connection changes.
```

App state (download list and settings) lives in `%APPDATA%\com.itsblqz.vibedownloader\downloads.json`.

## Roadmap ideas

- Browser extension to capture downloads
- Clipboard monitoring
- Global and per-download speed limits
- Scheduler (start and stop at set times)
- Checksum verification (SHA-256) after download
- Mirror and multi-source downloads
- Tray icon and notifications on completion
