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

  // Pick-and-place: picking an item makes the gaps it could go to into
  // targets, without moving anything on screen.
  const gaps = page.locator('.gutter');
  const gap = (name) => page.getByRole('button', { name, exact: true });
  const tops = () => Promise.all([0, 1, 2].map(async (i) => (await items(page).nth(i).boundingBox()).y));
  const before = await tops();
  await page.getByLabel('Move', { exact: true }).nth(2).click();
  expect(await tops()).toEqual(before);
  // Not the gaps either side of it: dropping there would do nothing.
  await expect(gaps).toHaveCount(2);
  await expect(gap('Move to the top')).toBeVisible();
  await expect(gap('Move between “a” and “b”')).toBeVisible();
  // The rows' own controls are out of reach while moving.
  await page.locator('ul.items > li[data-id]').nth(0).click({ position: { x: 20, y: 22 }, force: true });
  await expect(page.getByLabel('Done').nth(0)).not.toBeChecked();
  await page.keyboard.press('Escape');
  await expect(gaps).toHaveCount(0);
  expect(await tops()).toEqual(before);

  // The item lands where the tapped gap was.
  await page.getByLabel('Move', { exact: true }).nth(2).click();
  const target = await gap('Move between “a” and “b”').boundingBox();
  await gap('Move between “a” and “b”').click();
  await expectItems(page, ['a', 'c', 'b']);
  await expect(gaps).toHaveCount(0);
  const landed = await items(page).nth(1).boundingBox();
  expect(Math.abs(landed.y + landed.height / 2 - (target.y + target.height / 2))).toBeLessThan(30);

  await page.getByLabel('Move', { exact: true }).nth(0).click();
  await gap('Move between “c” and “b”').click();
  await expectItems(page, ['c', 'a', 'b']);

  await page.getByLabel('Move', { exact: true }).nth(1).click();
  await gap('Move to the end').click();
  await expectItems(page, ['c', 'b', 'a']);
  await page.getByLabel('Move', { exact: true }).nth(2).click();
  await gap('Move between “c” and “b”').click();
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

test('moves an item from anywhere in a long list to either end', async ({ page }) => {
  await createList(page, 'Long');
  const texts = Array.from({ length: 20 }, (_, i) => `item ${i}`);
  for (const text of texts) {
    await addItem(page, text);
  }
  const pick = (i) => page.getByLabel('Move', { exact: true }).nth(i).click();
  const gap = (name) => page.getByRole('button', { name, exact: true });

  // The page scrolls as usual while moving.
  await pick(12);
  await page.mouse.move(640, 300);
  await page.mouse.wheel(0, -5000);
  await expect.poll(() => page.evaluate(() => scrollY)).toBe(0);
  // Rows scrolled under the sheet stay under it, drop targets included.
  const covered = await page.evaluate(() => {
    const sheet = document.querySelector('.mover').getBoundingClientRect();
    // Scroll a drop target to the middle of the sheet.
    const below = [...document.querySelectorAll('.gutter')]
      .map((g) => g.getBoundingClientRect())
      .find((r) => r.top > sheet.top);
    if (!below) return 'no drop target below the sheet to scroll under it';
    scrollBy(0, below.top + below.height / 2 - (sheet.top + sheet.height / 2));
    const hits = [];
    for (let y = sheet.top + 2; y < sheet.bottom; y += 4) {
      for (let x = sheet.left + 10; x < sheet.right; x += 20) {
        hits.push(document.elementFromPoint(x, y)?.closest('.mover') != null);
      }
    }
    return hits.every(Boolean);
  });
  expect(covered).toBe(true);
  await page.mouse.wheel(0, -5000);
  await expect.poll(() => page.evaluate(() => scrollY)).toBe(0);
  await gap('Move to the top').click();
  await expect(items(page).first()).toHaveValue('item 12');

  await pick(3);
  await page.mouse.wheel(0, 5000);
  await gap('Move to the end').click();
  await expect(items(page).last()).toHaveValue('item 2');
});

test('the add sheet closes only from outside it, and forgets abandoned text', async ({ page }) => {
  await createList(page, 'Sheet');
  const sheet = page.locator('dialog.add-sheet');
  await page.getByRole('button', { name: 'Add item' }).click();
  await page.getByLabel('New item').fill('eggs');

  // Taps on the sheet's own padding don't close it.
  const box = await sheet.boundingBox();
  await page.mouse.click(box.x + 4, box.y + box.height - 4);
  await expect(sheet).toBeVisible();
  // A tap on the backdrop does.
  await page.mouse.click(box.x + box.width / 2, box.y - 40);
  await expect(sheet).toBeHidden();

  // What was typed and left doesn't come back.
  await page.getByRole('button', { name: 'Add item' }).click();
  await expect(page.getByLabel('New item')).toHaveValue('');
});

test('an unknown address still has the title bar', async ({ page }) => {
  await page.goto('/no/such/page');
  await expect(page.getByText('Not found.')).toBeVisible();
  await expect(page.getByRole('button', { name: /^Account:/ })).toBeVisible();
  await page.getByRole('link', { name: 'All lists' }).click();
  await expect(page.getByLabel('New list title')).toBeVisible();
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
