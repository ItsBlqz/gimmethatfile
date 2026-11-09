// Renders app/icon.svg into the PNG sizes the extension manifest needs.
// Run with `npm run icons` after changing the app icon.
import { chromium } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const svg = await readFile(path.join(here, "../../app/icon.svg"), "utf8");
const browser = await chromium.launch();
const page = await browser.newPage();
for (const size of [16, 32, 48, 128]) {
  await page.setViewportSize({ width: size, height: size });
  await page.setContent(
    `<style>html,body{margin:0;background:transparent}svg{display:block;width:${size}px;height:${size}px}</style>${svg}`,
  );
  await page.screenshot({
    path: path.join(here, `../icons/${size}.png`),
    omitBackground: true,
  });
  console.log(`icons/${size}.png`);
}
await browser.close();
