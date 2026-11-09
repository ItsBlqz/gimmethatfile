// Shared by the background script, the popup and the options page.
// Plain script (no modules) so it works as a Chrome service worker
// (importScripts) and as a Firefox background script.

/* exported GTF */
var GTF = (() => {
  const api = typeof browser !== "undefined" && browser.runtime ? browser : chrome;

  // The app's local receiver. Fixed port, see app/src/browser.rs.
  const BASE = "http://127.0.0.1:47811";

  const DEFAULTS = {
    // Capture downloads at all (the toolbar toggle).
    enabled: true,
    // Smaller downloads stay in the browser. 0 = capture everything.
    minSizeKB: 0,
    // Comma/space separated extensions, e.g. "zip iso exe". Empty = all.
    includeExt: "",
    // Never capture these extensions.
    excludeExt: "",
    // One host per line; subdomains match too.
    skipSites: "",
  };

  async function getConfig() {
    const stored = await api.storage.local.get(DEFAULTS);
    return { ...DEFAULTS, ...stored };
  }

  function setConfig(patch) {
    return api.storage.local.set(patch);
  }

  async function request(path, { method = "GET", body, timeout = 1500 } = {}) {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), timeout);
    try {
      const res = await fetch(BASE + path, {
        method,
        headers: body ? { "Content-Type": "application/json" } : {},
        body: body ? JSON.stringify(body) : undefined,
        signal: ctrl.signal,
        cache: "no-store",
        credentials: "omit",
      });
      let data = null;
      try {
        data = await res.json();
      } catch {}
      return { ok: res.ok, status: res.status, data };
    } finally {
      clearTimeout(timer);
    }
  }

  // { state: "ready" | "disabled" | "offline", version? }
  async function ping() {
    try {
      // POST, not GET: Chrome only sends our extension Origin (which the app
      // requires) on non-GET requests.
      const r = await request("/v1/ping", { method: "POST", body: {} });
      if (!r.ok || !r.data || r.data.app !== "Gimme That File") return { state: "offline" };
      return { state: r.data.capture ? "ready" : "disabled", version: r.data.version };
    } catch {
      return { state: "offline" };
    }
  }

  // Hand one download to the app. Resolves to true only when the app took it.
  async function send(payload) {
    try {
      const r = await request("/v1/downloads", { method: "POST", body: payload, timeout: 5000 });
      return r.status === 201;
    } catch {
      return false;
    }
  }

  function words(list) {
    return String(list || "")
      .toLowerCase()
      .split(/[\s,;]+/)
      .map((w) => w.replace(/^\*?\./, ""))
      .filter(Boolean);
  }

  function hostOf(url) {
    try {
      return new URL(url).hostname.toLowerCase();
    } catch {
      return "";
    }
  }

  return { api, BASE, DEFAULTS, getConfig, setConfig, ping, send, words, hostOf };
})();
