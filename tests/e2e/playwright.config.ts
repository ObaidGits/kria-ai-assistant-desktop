import { defineConfig, devices } from "@playwright/test";

const tauriMockUiUrl = process.env.KRIA_UI_URL || "http://127.0.0.1:1420";
const startTauriMockUi = process.env.KRIA_E2E_START_UI === "1";

export default defineConfig({
  testDir: "./tests",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: process.env.CI ? "github" : "html",
  timeout: 30_000,

  use: {
    // Base URL for the KRIA API server
    baseURL: process.env.KRIA_BASE_URL || "http://127.0.0.1:8088",
    trace: "on-first-retry",
    screenshot: "only-on-failure",
  },

  projects: [
    {
      name: "api-integration",
      testMatch: /.*\.api\.spec\.ts/,
    },
    {
      name: "e2e-chromium",
      use: { ...devices["Desktop Chrome"] },
      testMatch: /.*\.e2e\.spec\.ts/,
      testIgnore: /.*tauri-mock\.e2e\.spec\.ts/,
    },
    {
      name: "e2e-firefox",
      use: { ...devices["Desktop Firefox"] },
      testMatch: /.*\.e2e\.spec\.ts/,
      testIgnore: /.*tauri-mock\.e2e\.spec\.ts/,
    },
    {
      name: "e2e-tauri-mock",
      use: { ...devices["Desktop Chrome"] },
      testMatch: /.*tauri-mock\.e2e\.spec\.ts/,
    },
  ],

  webServer: startTauriMockUi
    ? {
        command: "cd ../../ui && npm run dev -- --host 127.0.0.1 --port 1420",
        url: tauriMockUiUrl,
        reuseExistingServer: !process.env.CI,
        timeout: 180_000,
      }
    : undefined,

  /* Optionally start the KRIA dev server before running tests */
  // webServer: {
  //   command: "cd ../.. && cargo run -p kria-server",
  //   url: "http://127.0.0.1:8088/api/health",
  //   reuseExistingServer: !process.env.CI,
  //   timeout: 120_000,
  // },
});
