import { defineConfig } from "@playwright/test";

// The tests share one fixed port (the app's receiver), so they run one at a time.
export default defineConfig({
  testDir: "tests",
  workers: 1,
  fullyParallel: false,
  timeout: 90_000,
  reporter: [["list"]],
});
