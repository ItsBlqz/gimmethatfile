# AGENTS.md

Guidance for AI coding agents (and humans in a hurry) working on Gimme That
File. [CONTRIBUTING.md](CONTRIBUTING.md) and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) are the full references; this file
is the short version plus the traps that aren't obvious from the code.

## What this is

A Windows download manager: multi-connection HTTP downloads with pause/resume,
a queue, speed limits, a scheduler and a browser extension that hands downloads
to the app.

| Path | What | Crate / tooling |
|---|---|---|
| `crates/engine/` | The download engine. All download logic lives here. | `gtf-engine` |
| `app/` | Tauri v2 shell: window, tray, Tauri commands, browser receiver (`browser.rs`). | `gimmethatfile` |
| `ui/` | Plain HTML/CSS/JS. No framework, no bundler, no npm. | — |
| `extension/` | Manifest V3 browser extension; Playwright tests in `extension/test/`. | npm (tests only) |
| `scripts/` | Benchmark comparison and repo maintenance scripts (bash + Python stdlib). | — |

## Commands

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                       # all Rust tests; no network needed
cargo test -p gtf-engine --test e2e          # one test file
cargo run -p gimmethatfile                   # run the app (debug)
cargo deny check                             # after adding or updating a dependency
scripts/perf_ab.sh origin/main               # speed check for engine changes (bash, Python 3)
```

The extension tests need a debug build of the app and Chromium; see
[extension/test/README.md](extension/test/README.md).

## Rules

- **Never touch the user's real data.** Set `GIMMETHATFILE_DATA_DIR` to a temp
  folder whenever you run the app or anything that creates a `Manager`. Without
  it, the app reads and writes the real download list.
- **The app is single-instance** and owns port `127.0.0.1:47811`. If a copy is
  already running, `cargo run -p gimmethatfile` just shows that window and
  exits. Close it first (from the tray: Quit), and don't kill processes you
  didn't start.
- **Keep the engine free of UI code.** The app talks to it only through
  `Manager`, `Snapshot` and events.
- **Tests never use the internet.** Reproduce a server behavior in the
  in-process test servers (`crates/engine/tests/`, `examples/common/testserver.rs`)
  and show the test fails without the fix.
- **Respect the invariants** in docs/ARCHITECTURE.md ("Invariants"): segment
  lock order, `pos` only counts bytes already on disk, worker-count accounting.
  Update that section if you change them.
- **Security-sensitive code:** `app/src/browser.rs` (origin/host checks, no
  target folder from requests), `crates/engine/src/headers.rs` (which headers
  a download may carry; cookies are never logged) and `crates/engine/src/naming.rs`
  (file names from servers must never escape the download folder). Per-download
  headers such as cookies only go to the link's own origin. Read
  [SECURITY.md](SECURITY.md) before changing any of this.
- **New dependencies** must pass `cargo deny check` (licences in `deny.toml`).
  The UI stays dependency-free.
- **User-visible changes** go in `CHANGELOG.md` under `Unreleased`. The version
  lives only in the root `Cargo.toml`.

## Performance

Speed is a feature and CI enforces it: the required **Performance** check
compares a PR against `main` on simulated servers and fails if it is slower by
more than 3 standard errors and 5%. It only measures when `crates/engine/`,
`Cargo.toml`/`Cargo.lock` or the benchmark scripts change.

- Run `scripts/perf_ab.sh origin/main` before pushing engine changes. It takes
  several minutes; on Windows run it in Git Bash.
- Changes to the connection controller (`control.rs`) must survive the `noisy`
  scenario: it reproduces a real regression where reacting to throughput noise
  halved speed. Reducing connections based on throughput is deliberately off by
  default (`GTF_ADAPTIVE_DOWN=1` enables it).
- Local benchmark numbers say "did this get slower", not "how fast is it for
  users". Don't put local numbers in docs as speed claims.

## Gotchas

- `cargo clippy` and `cargo check` don't produce an executable. To try an app
  change, run or build it.
- On Windows, `python3` may be the Microsoft Store stub; the scripts pick a
  Python that actually runs.
- `GTF_DEBUG=1` logs retries, throttling and connection-count changes to stderr.

## Commits and pull requests

- Commit titles: short, imperative, about the change itself ("Fix resume after
  ETag change"). The body explains why. Reference issues with `Fixes #123`.
- One change per PR. `main` is protected: open a PR; CI and a maintainer review
  are required.
- If an agent helped, a `Co-Authored-By:` trailer is welcome, but you're
  responsible for every line: run the commands above and read the diff.
