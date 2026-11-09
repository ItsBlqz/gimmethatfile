# Extension tests

Playwright tests that load the unpacked extension into Chromium and run it
against the real app (debug build).

```sh
cargo build -p gimmethatfile          # from the repo root; the tests start target/debug/gimmethatfile
cd extension/test
npm install
npx playwright install chromium
npm test
```

Close Gimme That File before running them: the app is single-instance and the
tests need its port (`127.0.0.1:47811`). The tests start the app with
`GIMMETHATFILE_DATA_DIR` set to a fresh temporary folder and a download folder
inside it, so your real download list and Downloads folder are never touched.
Set `GTF_APP_EXE` to test a different build, and `HEADED=1` to watch the
browser.

What they check:

| Test | Verifies |
|---|---|
| a cookie-protected download is handed to the app | Clicking a link to a file that needs a login cookie: the app gets it, every request it makes carries the cookie, referrer and browser User-Agent, the file's bytes match, the browser's copy is cancelled and erased, and the cookie never shows up in the app's log. |
| without the app, the browser downloads the file itself | Fallback when nothing listens on the port. |
| with capture turned off in the app, the browser keeps the download | Fallback when the app says `capture: false`. |
| downloads below the minimum size stay in the browser | The options filter. |
| a web page cannot add downloads | `fetch` from a normal page (JSON with preflight, a "simple" `no-cors` POST, and the ping) is refused, and so are direct requests with a web origin or none. |
