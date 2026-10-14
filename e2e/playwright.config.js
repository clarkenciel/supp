// Browser tests against the trunk dev server.
const { defineConfig, devices } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  timeout: 30_000,
  use: {
    baseURL: 'http://127.0.0.1:8080',
    ...devices['Desktop Chrome'],
  },
  webServer: {
    command: 'trunk serve --address 127.0.0.1 --port 8080',
    cwd: '../crates/web',
    url: 'http://127.0.0.1:8080',
    reuseExistingServer: true,
    timeout: 600_000,
  },
});
