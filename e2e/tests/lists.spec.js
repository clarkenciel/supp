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
  // Rows stay where they are on screen as a move starts and ends.
  const tops = () => Promise.all([0, 2].map(async (i) => (await items(page).nth(i).boundingBox()).y));
  const before = await tops();
  await page.getByLabel('Move', { exact: true }).nth(2).click();
  await expect(hint).toHaveText('Where it is now');
  expect(await tops()).toEqual(before);
  await page.keyboard.press('Escape');
  await expect(dropHere).toHaveCount(0);
  expect(await tops()).toEqual(before);

  await page.getByLabel('Move', { exact: true }).nth(2).click();
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

  // Tapping a row puts the zone above it (top half) or below (bottom
  // half), without reaching the row's controls.
  const rows = page.locator('ul.items > li[data-id]');
  await page.getByLabel('Move', { exact: true }).nth(2).click();
  await rows.nth(0).click({ position: { x: 100, y: 5 } });
  await expect(hint).toHaveText('At the top');
  await rows.nth(0).click({ position: { x: 100, y: 35 } });
  await expect(hint).toHaveText('Between “c” and “a”');
  await rows.nth(2).click({ position: { x: 5, y: 5 } });
  await expect(hint).toHaveText('Where it is now');
  await expect(page.getByLabel('Done').nth(0)).not.toBeChecked();
  await rows.nth(0).click({ position: { x: 100, y: 5 } });
  await dropHere.click();
  await expectItems(page, ['b', 'c', 'a']);

  // Swiping or the wheel moves the zone a gap per 64px, even with nothing
  // to scroll.
  await page.getByLabel('Move', { exact: true }).nth(2).click();
  await page.mouse.move(640, 300);
  await page.mouse.wheel(0, -64);
  await expect(hint).toHaveText('Between “b” and “c”');
  await page.mouse.wheel(0, -64);
  await expect(hint).toHaveText('At the top');
  await page.mouse.wheel(0, -500);
  await expect(hint).toHaveText('At the top');
  await page.mouse.wheel(0, 128);
  await expect(hint).toHaveText('Where it is now');
  await page.keyboard.press('Escape');

  await items(page).nth(2).fill('apples');
  await items(page).nth(2).press('Enter');
  await page.getByLabel('Delete item').nth(0).click();
  await expectItems(page, ['c', 'apples']);

  await page.getByLabel('Done').nth(0).check();
  await page.getByRole('button', { name: 'Clear done' }).click();
  await expectItems(page, ['apples']);

  await page.waitForTimeout(300);
  await page.reload();
  await expectItems(page, ['apples']);
});

test('moves an item from anywhere in a long list to either end', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await createList(page, 'Long');
  const texts = Array.from({ length: 20 }, (_, i) => `item ${i}`);
  for (const text of texts) {
    await addItem(page, text);
  }
  const hint = page.locator('.drop-zone .hint');
  const pick = (i) => page.getByLabel('Move', { exact: true }).nth(i).click();

  // No room is added around the list: scrolling, taps and keys get the
  // zone everywhere.
  const room = () => page.locator('ul.items').evaluate((ul) => ul.style.paddingTop + ul.style.paddingBottom);
  await pick(12);
  await expect(hint).toHaveText('Where it is now');
  expect(await room()).toBe('');
  await page.mouse.move(640, 300);
  await page.mouse.wheel(0, -64 * 3);
  await expect(hint).toHaveText('Between “item 8” and “item 9”');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await expect(hint).toHaveText('Where it is now');

  // A swipe (here by mouse; touch is the same pointer events) moves it
  // too, and the click that ends it doesn't count as a tap on a row.
  await page.mouse.move(640, 400);
  await page.mouse.down();
  await page.mouse.move(640, 272, { steps: 8 });
  await page.waitForTimeout(150);
  await page.mouse.up();
  await expect(hint).toHaveText('Between “item 14” and “item 15”');
  for (let i = 0; i < 14; i += 1) await page.keyboard.press('ArrowUp');
  await expect(hint).toHaveText('At the top');
  await page.screenshot({ path: process.env.SHOTS ? `${process.env.SHOTS}/top.png` : undefined });
  for (let i = 0; i < 19; i += 1) await page.keyboard.press('ArrowDown');
  await expect(hint).toHaveText('At the end');
  await page.screenshot({ path: process.env.SHOTS ? `${process.env.SHOTS}/end.png` : undefined });
  await page.getByRole('button', { name: /^Drop here/ }).click();
  await expectItems(page, [...texts.filter((t) => t !== 'item 12'), 'item 12']);

  await pick(3);
  expect(await room()).toBe('');
  await page.screenshot({ path: process.env.SHOTS ? `${process.env.SHOTS}/entry.png` : undefined });
  for (let i = 0; i < 3; i += 1) await page.keyboard.press('ArrowUp');
  await expect(hint).toHaveText('At the top');
  await page.getByRole('button', { name: /^Drop here/ }).click();
  await expect(items(page).first()).toHaveValue('item 3');
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
