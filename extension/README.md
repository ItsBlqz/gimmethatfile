# Gimme That File browser extension

Sends your browser's downloads to the Gimme That File app, so they get
multiple connections, pause/resume and the app's queue. Manifest V3; works in
Chrome and Edge (and other Chromium browsers). Firefox support is prepared but
not tested yet (see below).

## Install (unpacked, for now)

Store listings come later. Until then:

1. Start Gimme That File (it has to be running to receive downloads).
2. Open `chrome://extensions` (Edge: `edge://extensions`).
3. Turn on **Developer mode**.
4. Click **Load unpacked** and pick this `extension` folder.
5. Pin the extension and click it: it should say **Connected**.

After pulling changes, click the reload icon on the extension's card.

## What it does

- **New downloads go to the app.** When the browser starts a download, the
  extension pauses it, checks that the app is running and accepts downloads,
  sends the link to the app, and only then cancels the browser's copy and
  removes it from the browser's download list.
- **Nothing gets lost.** If the app isn't running, capture is off in the app's
  settings, or anything goes wrong, the browser download is resumed (or started
  again) and the browser saves the file as usual.
- **Downloads that need a login work.** The extension sends the cookies the
  browser has for the file's site, the page it came from (referrer) and the
  browser's file name. When it sends cookies it also sends the browser's
  User-Agent, because sites often tie a session to it; otherwise the app uses
  its own. The app keeps these in its local download list so the download can
  be resumed later.
- **Right-click → Download with Gimme That File** on links, images, video and
  audio sends that file to the app directly.

### Popup and options

- The toolbar popup has an on/off switch, shows whether the app was found
  (**Connected** / **App not running** / **Capture is off in the app**), and
  has **Let the next download through** to hand one download to the browser.
  When the switch is off, the icon shows an `off` badge.
- **Options:** minimum file size, file types to capture (only these / never
  these), and sites to skip (the file's site or the page's site; subdomains
  included).

### Never captured

- `blob:` and `data:` URLs (made by the page itself, the app can't fetch them),
  `file:` URLs.
- Downloads from private/incognito windows (separate cookie store).
- Downloads the extension started itself (fallbacks).
- Downloads that already finished before the extension could pause them (tiny
  files).

## How it talks to the app

The app listens on `http://127.0.0.1:47811` (localhost only):

| Request | Reply |
|---|---|
| `POST /v1/ping` | `200 {"app":"Gimme That File","version":"0.1.0","api":1,"capture":true}` |
| `POST /v1/downloads` with JSON `{url, filename?, referrer?, cookies?, user_agent?, headers?, dir?}` | `201 {"id":…}`, or `409 {"error":"capture_disabled"}` when capture is off in the app |

The app answers only requests whose `Origin` is a browser extension
(`chrome-extension://…` or `moz-extension://…`). Web pages can't add downloads
or even see whether the app is running. Chrome leaves out `Origin` on GET
requests from extensions, so the extension uses POST for the ping too. Details
are in [SECURITY.md](../SECURITY.md) and
[docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md#browser-integration).

## Permissions

| Permission | Why |
|---|---|
| `downloads` | See new downloads, pause/cancel/resume them. |
| `cookies` + host access to all sites | Read the cookies for a download's site, so downloads behind a login work in the app. Host access also lets it reach the app on `127.0.0.1`. |
| `storage` | Your options. |
| `contextMenus` | The "Download with Gimme That File" menu item. |

Cookies are sent only to the app on your own computer, never anywhere else.

## Firefox

The same folder is meant to load in Firefox 128+ (`about:debugging` → **This
Firefox** → **Load Temporary Add-on…** → pick `manifest.json`). Differences:

- Firefox runs `background.scripts` instead of the service worker; the
  manifest lists both.
- Firefox has no `downloads.onDeterminingFilename`, so the file name comes from
  the browser's chosen path (with a ` (1)` suffix removed) or from the server.
- In Firefox, host permissions of MV3 extensions are opt-in: allow
  "Access your data for all websites" in the add-on's permissions, or it can't
  read cookies or reach the app.
- Not tested yet: whether Firefox sends the `moz-extension://` `Origin` the app
  requires. The app accepts it; if Firefox doesn't send it, captures fall back
  to the browser (nothing is lost).

## Tests

End-to-end tests drive Chromium with this extension loaded against the real
app. See [test/README.md](test/README.md).

## Icons

`icons/*.png` are rendered from `app/icon.svg`. After changing the app icon,
run `npm run icons` in `extension/test`.
