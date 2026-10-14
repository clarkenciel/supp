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

  await page.getByLabel('Move up').nth(2).click();
  await expectItems(page, ['a', 'c', 'b']);
  await page.getByLabel('Move down').nth(0).click();
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
