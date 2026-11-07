# Contributing to Gimme That File

Thanks for helping out. Bug reports, server quirks we don't handle, UI polish,
docs and code are all welcome.

## Ways to contribute

- **Report a bug** with the [bug report form](../../issues/new?template=bug_report.yml).
  If a download fails on a specific server, the most useful thing you can give
  us is the output with `GTF_DEBUG=1` (see below). Please don't post private or
  signed links.
- **Suggest a feature** with the [feature request form](../../issues/new?template=feature_request.yml).
  Check the roadmap in the README and the existing issues first.
- **Send a pull request.** For anything bigger than a small fix, open an issue
  first so we can agree on the approach before you put time in.
- **Share benchmarks** from your network (see [docs/BENCHMARKS.md](docs/BENCHMARKS.md)).

## Development setup

You need:

- [Rust](https://rustup.rs), stable, 1.90 or newer
- **Windows:** Visual Studio Build Tools (the "Desktop development with C++"
  workload). WebView2 is already part of Windows 10/11.
- **Linux:** the [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/#linux),
  needed only for the app. The engine builds anywhere.
- **macOS:** Xcode Command Line Tools

```sh
git clone https://github.com/ItsBlqz/gimmethatfile
cd gimmethatfile
cargo run -p gimmethatfile          # start the app
cargo test -p gtf-engine              # run the engine tests
```

The UI is plain HTML/CSS/JS in `ui/`, with no Node or bundler. Edit and restart
the app.

### Useful environment variables

| Variable | Effect |
|---|---|
| `GIMMETHATFILE_DATA_DIR=<dir>` | Use a different folder for the download list and settings, so you don't touch your real one. |
| `GTF_DEBUG=1` | Log retries, throttling and connection-count changes to stderr. |

### Trying the engine without the UI

```sh
cargo run --release -p gtf-engine --example get -- <url> 8 ./out
cargo run --release -p gtf-engine --example bench           # local, reproducible
```

## Before you open a pull request

CI runs these, so run them locally first:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Also:

- **Add a test** for any engine behavior change. If you're fixing a server
  quirk, teach the test server in `crates/engine/tests/e2e.rs` to behave the
  same way, and show that the test fails without your fix.
- **Keep the engine free of UI code.** Anything the UI needs goes through
  `Manager` and `Snapshot`.
- **Update `CHANGELOG.md`** under `Unreleased` if users will notice the change.
- If you change the download algorithm, read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
  especially the invariants, and update it if they change.
- Keep PRs focused: one change per PR is much easier to review.

## Pull request flow

`main` is protected: nobody pushes to it directly, maintainers included.

1. Fork the repo (maintainers can use a branch), and create a branch such as
   `fix/resume-etag`.
2. Open a pull request against `main`.
3. CI must pass (Lint, Test on Windows and Linux, MSRV, app build), and a
   maintainer has to approve the PR.
4. A maintainer merges it, usually with "Squash and merge".

The rules live in [`.github/rulesets/main.json`](.github/rulesets/main.json)
and are applied with `scripts/protect-main.sh`. If you rename a CI job, update
the ruleset too, or PRs will wait forever for a check that never runs.

## Commit messages

Use a short summary line in the imperative mood ("Fix resume after ETag
change"), a blank line, then *why* the change was needed. Reference issues
with `Fixes #123`.

## Releasing (maintainers)

1. Bump `version` in the root `Cargo.toml` (`[workspace.package]`). It's the
   only place the version lives.
2. Move the `Unreleased` notes in `CHANGELOG.md` under a new
   `## [x.y.z] - YYYY-MM-DD` heading.
3. Commit, then tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
4. The **Release** workflow tests and builds the Windows installer and portable
   `.exe`, and creates a **draft** GitHub release with checksums and the
   changelog notes. Check it, then publish it.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating you agree to uphold it.

## License

By contributing, you agree that your contributions are dual licensed under the
MIT and Apache-2.0 licenses, as described in the [README](README.md#license),
without any additional terms or conditions.
