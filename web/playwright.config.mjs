import { defineConfig } from "@playwright/test";

const externalUrl = process.env.REVIEWQ_TEST_URL;

export default defineConfig({
  testDir: "./tests/browser",
  workers: 1,
  retries: 0,
  use: {
    ...(externalUrl && { baseURL: externalUrl }),
    // A production build registers a service worker, and page.route cannot see requests that
    // pass through one.
    serviceWorkers: "block",
    trace: "retain-on-failure",
  },
  webServer: externalUrl
    ? undefined
    : {
        command: "pnpm run dev",
        env: { REVIEWQ_WEB_PORT: "0" },
        wait: { stdout: /ReviewQ (?<playwright_test_base_url>http:\/\/127\.0\.0\.1:\d+\/)/ },
      },
});
