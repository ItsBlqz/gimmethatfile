// Test helpers: a local file server that needs a login cookie, the real app
// (debug build) with a throwaway data folder, and Chromium with the unpacked
// extension loaded.
import { chromium } from "@playwright/test";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import fs from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
export const repoRoot = path.resolve(here, "../../..");
export const extensionDir = path.join(repoRoot, "extension");
export const APP_PORT = 47811;
export const COOKIE = "session=abc123";

export function appExe() {
  if (process.env.GTF_APP_EXE) return process.env.GTF_APP_EXE;
  const exe = process.platform === "win32" ? "gimmethatfile.exe" : "gimmethatfile";
  return path.join(repoRoot, "target", "debug", exe);
}

export async function tempDir(prefix) {
  return fs.mkdtemp(path.join(os.tmpdir(), `gtf-${prefix}-`));
}

/** Deterministic pseudo-random bytes. */
export function makeData(len) {
  const buf = Buffer.alloc(len);
  let x = 0x2545f491;
  for (let i = 0; i < len; i++) {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    buf[i] = x & 0xff;
  }
  return buf;
}

/**
 * Serves:
 *   /login            sets the session cookie, links to the file
 *   /page             a page without login (for the web-origin test)
 *   /files/report.bin the file, as an attachment, only with the cookie;
 *                     supports Range like a real server
 */
export async function startFileServer(body) {
  const requests = [];
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, "http://x");
    if (url.pathname === "/login" || url.pathname === "/page") {
      const headers = { "Content-Type": "text/html" };
      if (url.pathname === "/login") headers["Set-Cookie"] = `${COOKIE}; Path=/; HttpOnly`;
      res.writeHead(200, headers);
      res.end(`<!doctype html><title>files</title><a id="dl" href="/files/report.bin">Download report</a>`);
      return;
    }
    if (url.pathname === "/files/report.bin") {
      requests.push({
        cookie: req.headers.cookie,
        ua: req.headers["user-agent"],
        referer: req.headers.referer,
        range: req.headers.range,
      });
      if (!(req.headers.cookie || "").split(/;\s*/).includes(COOKIE)) {
        res.writeHead(403, { "Content-Type": "text/plain" });
        res.end("login required");
        return;
      }
      const common = {
        "Content-Type": "application/octet-stream",
        "Content-Disposition": 'attachment; filename="report.bin"',
        "Accept-Ranges": "bytes",
      };
      const m = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || "");
      if (m) {
        const start = +m[1];
        const end = m[2] ? Math.min(+m[2], body.length - 1) : body.length - 1;
        res.writeHead(206, {
          ...common,
          "Content-Range": `bytes ${start}-${end}/${body.length}`,
          "Content-Length": end - start + 1,
        });
        res.end(body.subarray(start, end + 1));
      } else {
        res.writeHead(200, { ...common, "Content-Length": body.length });
        res.end(body);
      }
      return;
    }
    res.writeHead(404);
    res.end();
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const origin = `http://127.0.0.1:${server.address().port}`;
  return { origin, requests, close: () => new Promise((r) => server.close(r)) };
}

/** Raw request to the app's receiver with chosen headers (Node can set Origin). */
export function appRequest(pathname, { method = "GET", headers = {}, body } = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { host: "127.0.0.1", port: APP_PORT, path: pathname, method, headers, timeout: 2000 },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, body: data }));
      },
    );
    req.on("error", reject);
    req.on("timeout", () => req.destroy(new Error("timeout")));
    if (body) req.write(body);
    req.end();
  });
}

const EXT_ORIGIN = "chrome-extension://testtesttesttesttesttesttesttest";

export async function appIsListening() {
  try {
    const r = await appRequest("/v1/ping", { headers: { Origin: EXT_ORIGIN } });
    return r.status === 200;
  } catch {
    return false;
  }
}

/**
 * Start the real app with its own data folder. Its download folder is set
 * inside the temp folder too, so nothing touches the user's real files.
 */
export async function startApp({ capture = true } = {}) {
  if (await appIsListening()) {
    throw new Error(`something already answers on port ${APP_PORT}; close Gimme That File first`);
  }
  const exe = appExe();
  if (!existsSync(exe)) throw new Error(`${exe} not found; run \`cargo build -p gimmethatfile\` first`);
  const dataDir = await tempDir("app");
  const outDir = path.join(dataDir, "out");
  await fs.mkdir(outDir);
  const state = { settings: { download_dir: outDir, browser_capture: capture }, downloads: [], next_id: 1 };
  await fs.writeFile(path.join(dataDir, "downloads.json"), JSON.stringify(state));

  let log = "";
  const proc = spawn(exe, [], {
    env: { ...process.env, GIMMETHATFILE_DATA_DIR: dataDir },
    stdio: ["ignore", "pipe", "pipe"],
  });
  proc.stdout.on("data", (d) => (log += d));
  proc.stderr.on("data", (d) => (log += d));
  let exited = false;
  proc.on("exit", () => (exited = true));

  const deadline = Date.now() + 30_000;
  while (!(await appIsListening())) {
    if (exited) {
      throw new Error(
        "the app exited right away. Is another copy of Gimme That File running (single instance)?\n" + log,
      );
    }
    if (Date.now() > deadline) {
      proc.kill();
      throw new Error("the app's browser receiver did not come up\n" + log);
    }
    await new Promise((r) => setTimeout(r, 200));
  }

  return {
    dataDir,
    outDir,
    log: () => log,
    async state() {
      try {
        return JSON.parse(await fs.readFile(path.join(dataDir, "downloads.json"), "utf8"));
      } catch {
        return null; // mid-write
      }
    },
    async stop() {
      if (!exited) {
        const gone = new Promise((r) => proc.once("exit", r));
        proc.kill();
        await gone;
      }
    },
  };
}

/** Chromium with the unpacked extension; returns the context and its service worker. */
export async function launchBrowser() {
  const userDataDir = await tempDir("chromium");
  const context = await chromium.launchPersistentContext(userDataDir, {
    channel: "chromium", // new headless mode, which supports extensions
    headless: !process.env.HEADED,
    acceptDownloads: true,
    args: [`--disable-extensions-except=${extensionDir}`, `--load-extension=${extensionDir}`],
  });
  let [worker] = context.serviceWorkers();
  if (!worker) worker = await context.waitForEvent("serviceworker");
  return { context, worker };
}

export async function waitFor(fn, { timeout = 30_000, interval = 200, what = "condition" } = {}) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, interval));
  }
}
