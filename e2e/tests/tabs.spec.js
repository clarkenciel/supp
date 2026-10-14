// Several tabs of the app in one browser: each is its own Automerge peer
// over the same IndexedDB. Pages in one browser context are tabs.
const { test, expect } = require('@playwright/test');
const {
  items, expectItems, createList, addItem, goOffline,
} = require('./helpers');

const lists = (page) => page.locator('ul.lists li');

/** Undoes goOffline. */
async function goOnline(context) {
  await context.unroute('**');
  await context.setOffline(false);
}

/** Counts the records in one of the app's IndexedDB stores. */
function stored(page, store) {
  return page.evaluate(
    (name) =>
      new Promise((resolve, reject) => {
        const open = indexedDB.open('supp');
        open.onerror = () => reject(open.error);
        open.onsuccess = () => {
          const db = open.result;
          const count = db.transaction(name).objectStore(name).count();
          count.onerror = () => reject(count.error);
          count.onsuccess = () => {
            db.close();
            resolve(count.result);
          };
        };
      }),
    store,
  );
}

const texts = (page) => items(page).evaluateAll((els) => els.map((el) => el.value));

/** Adds items until the page's tab compacts the list, returning their texts. */
async function addUntilCompacted(page, prefix) {
  const added = [];
  let chunks = await stored(page, 'changes');
  for (let i = 0; i < 200; i += 1) {
    const text = `${prefix}${i}`;
    await page.getByLabel('New item').fill(text);
    await page.getByLabel('New item').press('Enter');
    // Not necessarily last: merging the other tab's items reorders them.
    await expect.poll(() => texts(page).then((t) => t.includes(text))).toBe(true);
    added.push(text);
    const now = await stored(page, 'changes');
    if (now < chunks) return added;
    chunks = now;
  }
  throw new Error('the list was never compacted');
}

test('edits in one tab show in the other at once, offline', async ({ context }) => {
  const a = await context.newPage();
  await createList(a, 'Shared');
  await addItem(a, 'milk');
  const b = await context.newPage();
  await b.goto(a.url());
  await expectItems(b, ['milk']);
  await goOffline(context);

  await addItem(a, 'eggs');
  await expectItems(b, ['milk', 'eggs']);
  await b.getByLabel('Done').first().check();
  await expect(a.getByLabel('Done').first()).toBeChecked();
  await b.getByLabel('List title').fill('Renamed');
  await b.getByLabel('List title').press('Enter');
  await expect(a.getByLabel('List title')).toHaveValue('Renamed');
});

test('lists created or deleted in one tab come and go in the other', async ({ context }) => {
  const a = await context.newPage();
  await a.goto('/');
  const b = await context.newPage();
  await b.goto('/');
  await goOffline(context);

  await a.getByLabel('New list title').fill('Errands');
  await a.getByRole('button', { name: 'Create' }).click();
  await expect(lists(b)).toHaveText([/Errands\s*0\/0/]);
  await addItem(a, 'post office');
  await expect(lists(b)).toHaveText([/Errands\s*0\/1/]);

  a.once('dialog', (dialog) => dialog.accept());
  await a.getByRole('button', { name: 'Delete list' }).click();
  await expect(lists(a)).toHaveCount(0);
  await expect(lists(b)).toHaveCount(0);
});

test('tabs editing one list keep all their changes through compaction', async ({ context }) => {
  test.setTimeout(120_000);
  const a = await context.newPage();
  await createList(a, 'Big');
  const b = await context.newPage();
  await b.goto(a.url());
  await expect(b.getByLabel('List title')).toHaveValue('Big');
  await goOffline(context);

  // Each tab compacts in turn, replacing the stored snapshot.
  const fromA = await addUntilCompacted(a, 'a');
  const fromB = await addUntilCompacted(b, 'b');
  await expect.poll(() => items(b).count()).toBeGreaterThanOrEqual(fromB.length);

  // Nothing either tab wrote is lost. (Back online only so the reload
  // doesn't depend on the service worker having taken control.)
  await goOnline(context);
  await b.reload();
  await expect(b.getByLabel('List title')).toHaveValue('Big');
  expect((await texts(b)).sort()).toEqual([...fromA, ...fromB].sort());
});
