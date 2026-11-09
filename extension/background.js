// Catches new browser downloads and hands them to Gimme That File.
//
// Flow for every new download:
//   1. Skip what we must not touch (blob:/data:, our own fallback downloads,
//      private windows, filters from the options page).
//   2. Pause it, so nothing more lands on disk while we ask the app.
//   3. Ask the app (GET /v1/ping, short timeout). Not running or capture off:
//      resume the browser download and stop here.
//   4. Send it (POST /v1/downloads) with cookies, referrer and file name.
//   5. Only when the app confirmed (201): cancel and erase the browser's copy.
//      Any failure: resume it, or start it again if it can't be resumed.
// The user's download is never lost: the browser keeps it until the app has it.

if (typeof importScripts === "function") importScripts("common.js");

const { api } = GTF;
const MENU_ID = "gtf-download";

// URLs we are about to download through the browser ourselves (fallbacks).
const bypass = new Map(); // url -> expiry time
function allowThrough(url) {
  bypass.set(url, Date.now() + 30_000);
}
function takeBypass(url) {
  const until = bypass.get(url);
  if (until === undefined) return false;
  bypass.delete(url);
  return until > Date.now();
}

// "Let the next download go to the browser" from the popup.
async function takeSkipNext() {
  const area = api.storage.session || api.storage.local;
  const { skipNextUntil = 0 } = await area.get("skipNextUntil");
  if (skipNextUntil > Date.now()) {
    await area.remove("skipNextUntil");
    return true;
  }
  return false;
}

function baseName(path) {
  return String(path || "").split(/[\\/]/).pop();
}

// The browser's suggested file name (from the `download` attribute,
// Content-Disposition or the URL), before it is made unique in the
// Downloads folder. Chrome only; elsewhere we use the final path's name.
const suggested = new Map(); // download id -> name
if (api.downloads.onDeterminingFilename) {
  api.downloads.onDeterminingFilename.addListener((item, suggest) => {
    if (item.filename) suggested.set(item.id, baseName(item.filename));
    if (suggested.size > 100) suggested.delete(suggested.keys().next().value);
    suggest(); // no opinion: keep the browser's choice
  });
}

function suggestedName(item) {
  const name = suggested.get(item.id);
  suggested.delete(item.id);
  // "report (1).zip" -> "report.zip": the app picks its own unique name.
  return name || baseName(item.filename).replace(/ \(\d+\)(?=\.[^.]*$|$)/, "");
}

function extOf(name) {
  const m = /\.([a-z0-9]{1,10})$/i.exec(name || "");
  return m ? m[1].toLowerCase() : "";
}

function urlFileName(url) {
  try {
    return decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
  } catch {
    return "";
  }
}

function siteSkipped(cfg, ...urls) {
  const sites = GTF.words(cfg.skipSites);
  return urls.some((u) => {
    const host = GTF.hostOf(u);
    return host && sites.some((s) => host === s || host.endsWith("." + s));
  });
}

// Should this download go to the app, judging by the options page?
function wanted(cfg, item, url) {
  if (siteSkipped(cfg, url, item.referrer)) return false;
  const ext = extOf(baseName(item.filename)) || extOf(urlFileName(url));
  const include = GTF.words(cfg.includeExt);
  if (include.length && !include.includes(ext)) return false;
  if (ext && GTF.words(cfg.excludeExt).includes(ext)) return false;
  const size = item.totalBytes > 0 ? item.totalBytes : item.fileSize > 0 ? item.fileSize : -1;
  // Unknown size: capture (most big downloads report one).
  if (cfg.minSizeKB > 0 && size >= 0 && size < cfg.minSizeKB * 1024) return false;
  return true;
}

async function cookieHeader(url) {
  try {
    const cookies = await api.cookies.getAll({ url });
    // Same order a browser uses: longer paths first.
    cookies.sort((a, b) => (b.path || "").length - (a.path || "").length);
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return "";
  }
}

async function payloadFor(url, { filename, referrer }) {
  const cookies = await cookieHeader(url);
  const p = { url };
  if (filename) p.filename = filename;
  if (referrer && /^https?:/i.test(referrer)) p.referrer = referrer;
  if (cookies) {
    p.cookies = cookies;
    // Sessions are often tied to the browser's User-Agent. Without cookies
    // the app keeps its own, honest one.
    p.user_agent = navigator.userAgent;
  }
  return p;
}

async function find(id) {
  try {
    const [item] = await api.downloads.search({ id });
    return item;
  } catch {
    return undefined;
  }
}

// Give the download back to the browser: resume it, or start it again.
async function letBrowserHave(item, paused) {
  if (!paused) return;
  try {
    await api.downloads.resume(item.id);
    return;
  } catch {}
  const now = await find(item.id);
  if (now && (now.state === "complete" || (now.state === "in_progress" && !now.paused))) return;
  const url = item.finalUrl || item.url;
  allowThrough(url);
  try {
    await api.downloads.download({ url });
  } catch (e) {
    console.warn("Gimme That File: could not restart the download", e);
  }
}

async function onCreated(item) {
  const url = item.finalUrl || item.url;
  if (!/^https?:/i.test(url)) return; // blob:, data:, file:, ...
  if (takeBypass(url) || takeBypass(item.url)) return;
  if (item.byExtensionId && item.byExtensionId === api.runtime.id) return;
  if (item.incognito) return; // different cookie store; leave private windows alone
  if (item.state && item.state !== "in_progress") return;

  const cfg = await GTF.getConfig();
  if (!cfg.enabled || !wanted(cfg, item, url)) return;
  if (await takeSkipNext()) return;

  let paused = false;
  try {
    await api.downloads.pause(item.id);
    paused = true;
  } catch {
    // Already finished (tiny file) or not pausable: leave it to the browser.
    return;
  }

  const status = await GTF.ping();
  if (status.state !== "ready") return letBrowserHave(item, paused);

  // Size and name are often known by now even if they weren't at creation.
  const now = (await find(item.id)) || item;
  if (now.state === "complete") return;
  if (!wanted(cfg, now, url)) return letBrowserHave(item, paused);

  const payload = await payloadFor(url, {
    filename: suggestedName(now),
    referrer: now.referrer || item.referrer,
  });
  if (!(await GTF.send(payload))) return letBrowserHave(item, paused);

  try {
    await api.downloads.cancel(item.id);
  } catch {}
  try {
    await api.downloads.erase({ id: item.id });
  } catch {}
  flash("✓", "#4f46e5");
}

api.downloads.onCreated.addListener((item) => {
  onCreated(item).catch((e) => console.warn("Gimme That File:", e));
});

// ---------- context menu ----------

async function createMenu() {
  try {
    await api.contextMenus.removeAll();
    api.contextMenus.create({
      id: MENU_ID,
      title: "Download with Gimme That File",
      contexts: ["link", "video", "audio", "image"],
    });
  } catch {}
}
api.runtime.onInstalled.addListener(createMenu);
api.runtime.onStartup && api.runtime.onStartup.addListener(createMenu);

api.contextMenus.onClicked.addListener((info) => {
  if (info.menuItemId !== MENU_ID) return;
  const url = info.linkUrl || info.srcUrl;
  if (!url || !/^https?:/i.test(url)) return;
  (async () => {
    const status = await GTF.ping();
    if (status.state === "ready") {
      const payload = await payloadFor(url, { referrer: info.pageUrl });
      if (await GTF.send(payload)) return flash("✓", "#4f46e5");
    }
    // App not there: download it in the browser instead.
    flash("!", "#dc2626");
    allowThrough(url);
    await api.downloads.download({ url });
  })().catch((e) => console.warn("Gimme That File:", e));
});

// ---------- toolbar badge ----------

let flashTimer = null;
function flash(text, color) {
  try {
    api.action.setBadgeBackgroundColor({ color });
    api.action.setBadgeText({ text });
    clearTimeout(flashTimer);
    flashTimer = setTimeout(updateBadge, 2500);
  } catch {}
}

async function updateBadge() {
  const { enabled } = await GTF.getConfig();
  try {
    api.action.setBadgeBackgroundColor({ color: "#6b6b76" });
    api.action.setBadgeText({ text: enabled ? "" : "off" });
  } catch {}
}

api.storage.onChanged.addListener((changes) => {
  if ("enabled" in changes) updateBadge();
});
updateBadge();
