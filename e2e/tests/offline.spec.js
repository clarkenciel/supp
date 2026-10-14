// The service worker: starting with no network, and switching to new
// versions of the app.
const fs = require('node:fs');
const path = require('node:path');
const { test, expect } = require('@playwright/test');
const {
  invite, uniqueName, expectItems, createList, addItem, goOffline,
} = require('./helpers');

const swPath = path.join(__dirname, '..', '..', 'crates', 'web', 'dist', 'sw.js');

/** Waits for the service worker to be active, then reloads so it controls the page. */
async function installApp(page) {
  await page.goto('/');
  await page.evaluate(() => navigator.serviceWorker.ready);
  await page.reload();
  expect(await page.evaluate(() => !!navigator.serviceWorker.controller)).toBe(true);
}

const shellCaches = (page) =>
  page.evaluate(async () => (await caches.keys()).filter((k) => k.startsWith('supp-shell-')));

test('starts with no network once loaded', async ({ page, context }) => {
  await installApp(page);
  await createList(page, 'Camping');
  await addItem(page, 'tent');
  const listUrl = page.url();
  await page.waitForTimeout(300);

  await goOffline(context);
  // A deep link: the worker answers every client route with the shell.
  const response = await page.goto(listUrl);
  expect(response.fromServiceWorker()).toBe(true);
  await expectItems(page, ['tent']);
  await page.goto('/');
  await expect(page.locator('ul.lists li')).toHaveText([/Camping/]);

  // Server routes are never answered from the cache.
  expect(await page.evaluate(() => fetch('/api/me').then(() => 'ok', () => 'failed'))).toBe(
    'failed',
  );
});

test.describe('updates', () => {
  // These tests publish a "new build" by changing the served sw.js.
  test.describe.configure({ mode: 'serial' });
  let original;
  test.beforeAll(() => {
    original = fs.readFileSync(swPath, 'utf8');
  });
  test.afterEach(() => fs.writeFileSync(swPath, original));

  /** Makes the server's sw.js a different version, as a new build would. */
  function publishNewVersion() {
    const version = uniqueName('e2e');
    fs.writeFileSync(swPath, original.replace(/^const VERSION = .*$/m, `const VERSION = '${version}';`));
    return `supp-shell-${version}`;
  }

  test('offers a new version and switches to it on reload', async ({ page }) => {
    await installApp(page);
    const oldCaches = await shellCaches(page);
    expect(oldCaches).toHaveLength(1);

    const newCache = publishNewVersion();
    // Navigating makes the browser check for a new worker; it installs in
    // the background while this page keeps the old one.
    await page.reload();
    const reload = page.getByRole('button', { name: 'Reload' });
    await expect(page.getByText('A new version of the app is available.')).toBeVisible();
    expect(await page.evaluate(() => navigator.serviceWorker.controller.state)).toBe('activated');

    await Promise.all([page.waitForEvent('load'), reload.click()]);
    await expect(page.getByLabel('New list title')).toBeVisible();
    await expect(page.getByText('A new version of the app is available.')).toBeHidden();
    // The new worker controls the page, and the old cache is gone.
    await expect.poll(() => shellCaches(page)).toEqual([newCache]);
  });

  test('reload after the server refuses this version fetches the new one', async ({ page }) => {
    // Stand in for a server that no longer supports this client: the
    // socket answers Hello with UpgradeRequired once the test says so.
    let socket;
    await page.routeWebSocket('/sync', (ws) => {
      socket = ws;
    });
    const name = uniqueName('upgrader');
    await page.goto(invite());
    await page.getByLabel('Name').fill(name);
    await page.getByRole('button', { name: 'Sign in' }).click();
    await expect(page.locator('.sync-status')).toHaveText('Connecting…');
    await installApp(page);
    await expect(page.locator('.sync-status')).toHaveText('Connecting…');

    // The new build is out, but this page hasn't noticed: no prompt yet.
    const newCache = publishNewVersion();
    // ServerMessage { upgrade: UpgradeRequired { min_protocol_version: 2 } }
    socket.send(Buffer.from([0x2a, 0x02, 0x08, 0x02]));
    const reload = page.getByRole('button', { name: 'Reload' });
    await expect(page.getByText('A new version of the app is available.')).toBeVisible();

    await Promise.all([page.waitForEvent('load'), reload.click()]);
    await expect.poll(() => shellCaches(page)).toEqual([newCache]);
  });
});
