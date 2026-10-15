// Anonymous, offline-capable use: everything lives in IndexedDB.
const { test, expect } = require('@playwright/test');

const {
  items, expectItems, createList: create, addItem, goOffline,
} = require('./helpers');

async function createList(page, title, { navigate = true } = {}) {
  if (navigate) return create(page, title);
  await page.getByLabel('New list title').fill(title);
  await page.getByRole('button', { name: 'Create' }).click();
  await expect(page.getByLabel('List title')).toHaveValue(title);
}

test('creates a list and keeps it across reloads', async ({ page }) => {
  await createList(page, 'Groceries');
  for (const text of ['milk', 'eggs', 'bread']) {
    await addItem(page, text);
  }
  await page.getByLabel('Done').nth(1).check();

  // Writes are async; give IndexedDB a moment before reloading.
  await page.waitForTimeout(300);
  await page.reload();

  await expectItems(page, ['milk', 'eggs', 'bread']);
  await expect(page.getByLabel('Done').nth(1)).toBeChecked();

  await page.getByRole('link', { name: 'All lists' }).click();
  await expect(page.locator('ul.lists li')).toHaveText([/Groceries\s*1\/3/]);
});

test('reorders, edits and removes items', async ({ page }) => {
  await createList(page, 'Chores');
  for (const text of ['a', 'b', 'c']) {
    await addItem(page, text);
  }

  // Pick-and-place: the drop zone starts in the item's own gap, and the
  // arrow keys scroll it a gap at a time. Its pulse would keep Playwright
  // waiting for it to hold still.
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const hint = page.locator('.drop-zone .hint');
  const dropHere = page.getByRole('button', { name: /^Drop here/ });
  await page.getByLabel('Move', { exact: true }).nth(2).click();
  await expect(hint).toHaveText('Where it is now');
  await page.keyboard.press('ArrowUp');
  await expect(hint).toHaveText('Between “a” and “b”');
  await dropHere.click();
  await expectItems(page, ['a', 'c', 'b']);
  await expect(dropHere).toHaveCount(0);

  await page.getByLabel('Move', { exact: true }).nth(0).click();
  await page.keyboard.press('ArrowUp');
  await expect(hint).toHaveText('Where it is now');
  await page.keyboard.press('ArrowDown');
  await expect(hint).toHaveText('Between “c” and “b”');
  await dropHere.click();
  await expectItems(page, ['c', 'a', 'b']);

  await page.getByLabel('Move', { exact: true }).nth(1).click();
  await page.keyboard.press('ArrowDown');
  await expect(hint).toHaveText('At the end');
  await page.keyboard.press('Escape');
  await expect(dropHere).toHaveCount(0);
  await expectItems(page, ['c', 'a', 'b']);

  await items(page).nth(1).fill('apples');
  await items(page).nth(1).press('Enter');
  await page.getByLabel('Delete item').nth(2).click();
  await expectItems(page, ['c', 'apples']);

  await page.getByLabel('Done').nth(0).check();
  await page.getByRole('button', { name: 'Clear done' }).click();
  await expectItems(page, ['apples']);

  await page.waitForTimeout(300);
  await page.reload();
  await expectItems(page, ['apples']);
});

test('keeps working when the network goes away', async ({ page, context }) => {
  // Starting with no network is covered in offline.spec.js.
  await page.goto('/');
  await goOffline(context);
  await createList(page, 'Trip', { navigate: false });
  await addItem(page, 'tent');
  await expectItems(page, ['tent']);
});

test('deletes a list', async ({ page }) => {
  await createList(page, 'Scratch');
  page.once('dialog', (dialog) => dialog.accept());
  await page.getByRole('button', { name: 'Delete list' }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.locator('ul.lists li')).toHaveCount(0);

  await page.waitForTimeout(300);
  await page.reload();
  await expect(page.locator('ul.lists li')).toHaveCount(0);
});
