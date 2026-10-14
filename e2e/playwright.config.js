// Browser tests against a real server (dev identity provider) serving the
// built client, as in production.
const { defineConfig, devices } = require('@playwright/test');

// Lets tests route (and so cut off) the service worker's own requests.
process.env.PW_EXPERIMENTAL_SERVICE_WORKER_NETWORK_EVENTS = '1';

module.exports = defineConfig({
  testDir: './tests',
  timeout: 30_000,
  use: {
    baseURL: 'http://127.0.0.1:3100',
    ...devices['Desktop Chrome'],
  },
  webServer: {
    command: './server.sh',
    url: 'http://127.0.0.1:3100',
    reuseExistingServer: !process.env.CI,
    timeout: 600_000,
  },
});
