# Security Policy

## Supported versions

Only the latest release receives security fixes.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report them privately through GitHub:
**Security → Report a vulnerability** on this repository
([direct link](../../security/advisories/new)).

Include what you found, how to reproduce it (a URL or a small server
configuration that triggers it is ideal), and the impact you expect. We aim to
acknowledge reports within 7 days and will keep you updated until a fix is
released. We're happy to credit you in the release notes.

## Scope

Areas we especially care about:

- A server making the app write outside the chosen download folder, for example
  through a hostile `Content-Disposition` file name or redirects.
- A server making the app run or open a file without the user asking.
- Corrupted downloads reported as complete.
- Anything that lets web content in the UI reach Tauri commands it shouldn't.
- Anything that lets a web page talk to the browser receiver (below).

## Browser receiver

The app listens on `http://127.0.0.1:47811` so the browser extension can hand
it downloads. It is built so that only browser extensions can use it:

- It binds to `127.0.0.1` only, so other computers can't reach it.
- Every request needs an `Origin` header of `chrome-extension://…` or
  `moz-extension://…`. Browsers always attach a web page's real origin to its
  requests and don't let pages change it, so web pages are refused (`403`),
  and so are requests without an origin.
- The `Host` header must be `127.0.0.1:47811` or `localhost:47811`, which blocks
  DNS-rebinding attacks.
- CORS headers are only ever sent to extension origins, and only those get an
  answer to a CORS preflight. A page therefore can't read replies, not even
  whether the app is running.
- Adding a download requires `Content-Type: application/json` (which a page
  can't send cross-origin without a preflight), a body of at most 64 KiB, and an
  `http`/`https` URL.
- With **Settings → Browser → Accept downloads sent by the browser extension**
  turned off, it adds nothing and the extension leaves downloads to the browser.

Programs running on your computer can still fake an extension origin. That is
out of scope: local programs can already do anything your user account can.

### Cookies

To download files behind a login, the extension sends the browser's cookies for
the file's site (and then the browser's User-Agent) to the app. They are:

- sent only with requests for that download; when a redirect leaves the site,
  `Cookie` and `Authorization` are dropped;
- stored in plain text in the local download list (`downloads.json` in the
  app's data folder) only while the download is unfinished, so a paused or
  interrupted download can resume. They are deleted as soon as the download
  completes, or when it is removed from the list;
- never written to the `GTF_DEBUG` log (only header names are).

Headers that would change how the app talks to the server (`Host`, `Range`,
`Accept-Encoding`, `Connection`, `Content-Length` and similar) are ignored.

Downloaded files themselves are untrusted. Gimme That File doesn't scan them,
so scan files from sources you don't trust.
