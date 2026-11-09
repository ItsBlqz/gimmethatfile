import { expect, test } from "@playwright/test";
import fs from "node:fs/promises";
import path from "node:path";
import {
  APP_PORT,
  COOKIE,
  appIsListening,
  appRequest,
  launchBrowser,
  makeData,
  startApp,
  startFileServer,
  tempDir,
  waitFor,
} from "./helpers.js";

const BODY = makeData(3 * 1024 * 1024 + 321);

let server;
let app;
let browser;

test.beforeEach(async () => {
  server = await startFileServer(BODY);
});

test.afterEach(async ({}, testInfo) => {
  if (testInfo.status !== testInfo.expectedStatus && app) {
    console.log("--- app log ---\n" + app.log());
  }
  await browser?.context.close();
  await app?.stop();
  await server?.close();
  browser = app = undefined;
});

/** Downloads the browser still lists (the extension erases captured ones). */
function browserDownloads(worker) {
  return worker.evaluate(() =>
    chrome.downloads.search({}).then((items) => items.map((d) => ({ url: d.url, state: d.state }))),
  );
}

test("a cookie-protected download is handed to the app", async () => {
  app = await startApp();
  browser = await launchBrowser();
  const page = await browser.context.newPage();
  // Playwright normally renames browser downloads to GUIDs; keep real names
  // (still in a temp folder) so the browser's suggested name reaches the app.
  const cdp = await browser.context.newCDPSession(page);
  await cdp.send("Browser.setDownloadBehavior", {
    behavior: "allow",
    downloadPath: await tempDir("browser-dl"),
  });
  await page.goto(`${server.origin}/login`);
  const browserUA = await page.evaluate(() => navigator.userAgent);
  await page.click("#dl");

  const rec = await waitFor(
    async () => {
      const d = (await app.state())?.downloads?.[0];
      return d?.status === "completed" && d;
    },
    { what: "the app to finish the download" },
  );
  expect(rec.url).toBe(`${server.origin}/files/report.bin`);
  expect(rec.file_name).toBe("report.bin");
  const got = await fs.readFile(path.join(app.outDir, "report.bin"));
  expect(got.length).toBe(BODY.length);
  expect(got.equals(BODY)).toBe(true);

  // Cookies, referrer and the browser's UA went along and were kept for resume.
  const sent = Object.fromEntries(rec.headers);
  expect(sent.cookie).toContain(COOKIE);
  expect(sent.referer).toBe(`${server.origin}/login`);
  expect(sent["user-agent"]).toBe(browserUA);

  // Every request the app made carried them (the browser made the first one).
  const fromApp = server.requests.filter((r) => r.ua === browserUA && r.range);
  expect(fromApp.length).toBeGreaterThan(0);
  expect(server.requests.every((r) => (r.cookie || "").includes(COOKIE))).toBe(true);

  // The browser's own copy was cancelled and erased from its list.
  await waitFor(async () => (await browserDownloads(browser.worker)).length === 0, {
    what: "the browser download to be erased",
  });
  expect(app.log()).not.toContain("abc123"); // cookie values are never logged
});

test("without the app, the browser downloads the file itself", async () => {
  expect(await appIsListening(), `port ${APP_PORT} must be free for this test`).toBe(false);
  browser = await launchBrowser();
  const page = await browser.context.newPage();
  await page.goto(`${server.origin}/login`);
  const [download] = await Promise.all([page.waitForEvent("download"), page.click("#dl")]);
  expect(await download.failure()).toBeNull();
  const got = await fs.readFile(await download.path());
  expect(got.equals(BODY)).toBe(true);
  await waitFor(
    async () => (await browserDownloads(browser.worker)).some((d) => d.state === "complete"),
    { what: "the browser download to complete" },
  );
});

test("with capture turned off in the app, the browser keeps the download", async () => {
  app = await startApp({ capture: false });
  browser = await launchBrowser();
  const page = await browser.context.newPage();
  await page.goto(`${server.origin}/login`);
  const [download] = await Promise.all([page.waitForEvent("download"), page.click("#dl")]);
  expect(await download.failure()).toBeNull();
  const got = await fs.readFile(await download.path());
  expect(got.equals(BODY)).toBe(true);
  expect((await app.state()).downloads).toEqual([]);
});

test("downloads below the minimum size stay in the browser", async () => {
  app = await startApp();
  browser = await launchBrowser();
  await browser.worker.evaluate(() => chrome.storage.local.set({ minSizeKB: 100 * 1024 }));
  const page = await browser.context.newPage();
  await page.goto(`${server.origin}/login`);
  const [download] = await Promise.all([page.waitForEvent("download"), page.click("#dl")]);
  expect(await download.failure()).toBeNull();
  expect((await fs.readFile(await download.path())).equals(BODY)).toBe(true);
  expect((await app.state()).downloads).toEqual([]);
});

test("a web page cannot add downloads", async () => {
  app = await startApp();
  browser = await launchBrowser();
  const page = await browser.context.newPage();
  await page.goto(`${server.origin}/page`);
  const target = `${server.origin}/files/evil.bin`;

  const results = await page.evaluate(
    async ({ port, target }) => {
      const base = `http://127.0.0.1:${port}`;
      const out = {};
      const attempt = async (name, fn) => {
        try {
          const r = await fn();
          out[name] = `status ${r.status} (${r.type})`;
        } catch (e) {
          out[name] = "blocked";
        }
      };
      const body = JSON.stringify({ url: target });
      await attempt("json", () =>
        fetch(`${base}/v1/downloads`, { method: "POST", headers: { "Content-Type": "application/json" }, body }),
      );
      // A "simple" request skips the CORS preflight; the server must still refuse it.
      await attempt("simple", () =>
        fetch(`${base}/v1/downloads`, { method: "POST", mode: "no-cors", headers: { "Content-Type": "text/plain" }, body }),
      );
      await attempt("ping", () => fetch(`${base}/v1/ping`));
      return out;
    },
    { port: APP_PORT, target },
  );
  expect(results.json).toBe("blocked");
  expect(results.ping).toBe("blocked");
  expect(results.simple).toBe("status 0 (opaque)"); // can't read anything either

  // The same requests straight to the server: web origins and no origin are refused.
  const json = { "Content-Type": "application/json" };
  const body = JSON.stringify({ url: target });
  for (const headers of [{ ...json, Origin: server.origin }, json, { ...json, Origin: "null" }]) {
    const r = await appRequest("/v1/downloads", { method: "POST", headers, body });
    expect(r.status).toBe(403);
    expect(r.headers["access-control-allow-origin"]).toBeUndefined();
  }
  const pre = await appRequest("/v1/downloads", {
    method: "OPTIONS",
    headers: { Origin: server.origin, "Access-Control-Request-Method": "POST" },
  });
  expect(pre.status).toBe(403);
  expect(pre.headers["access-control-allow-origin"]).toBeUndefined();

  await new Promise((r) => setTimeout(r, 500));
  expect((await app.state()).downloads).toEqual([]);
  expect(server.requests.length).toBe(0);
});
