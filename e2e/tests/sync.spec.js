// Signed-in use: accounts, invites and sync between devices. Each browser
// context is a separate device.
const { test, expect } = require('@playwright/test');
const {
  admin, invite, userId, uniqueName, signIn, expectItems, createList, addItem,
} = require('./helpers');

const lists = (page) => page.locator('ul.lists li');

test('signing up needs an invite', async ({ page }) => {
  await page.goto('/auth/login');
  await page.getByLabel('Name').fill(uniqueName('stranger'));
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByText('You need an invite')).toBeVisible();
});

test('anonymous lists upload on first sign-in and sync to another device', async ({ browser }) => {
  const name = uniqueName('ann');
  const phone = await (await browser.newContext()).newPage();
  const laptop = await (await browser.newContext()).newPage();

  // Used anonymously first...
  await createList(phone, 'Groceries');
  await addItem(phone, 'milk');
  // ...then signed up with an invite.
  await signIn(phone, invite(), name);
  await expect(lists(phone)).toHaveText([/Groceries/]);

  // A second device with the same account gets the list.
  await signIn(laptop, '/auth/login', name);
  await expect(lists(laptop)).toHaveText([/Groceries/]);
  await lists(laptop).getByRole('link').click();
  await expectItems(laptop, ['milk']);

  // Edits flow both ways while both are open.
  await lists(phone).getByRole('link').click();
  await addItem(laptop, 'eggs');
  await expectItems(phone, ['milk', 'eggs']);
  await phone.getByLabel('Done').first().check();
  await expect(laptop.getByLabel('Done').first()).toBeChecked();
});

test('a list created right before navigating away still syncs', async ({ browser }) => {
  const name = uniqueName('quick');
  const page = await (await browser.newContext()).newPage();
  await signIn(page, invite(), name);
  // Regression: the list's record and contents used to be written
  // separately, so leaving at once could sync an empty 'Untitled' list.
  for (const title of ['One', 'Two', 'Three']) {
    await page.getByLabel('New list title').fill(title);
    await page.getByRole('button', { name: 'Create' }).click();
    await page.goto('/');
  }
  const other = await (await browser.newContext()).newPage();
  await signIn(other, '/auth/login', name);
  await expect(lists(other)).toHaveText([/One/, /Two/, /Three/]);
});

test('deleting a list on one device removes it from the others', async ({ browser }) => {
  const name = uniqueName('del');
  const a = await (await browser.newContext()).newPage();
  const b = await (await browser.newContext()).newPage();
  await signIn(a, invite(), name);
  await createList(a, 'Keep');
  await createList(a, 'Doomed');
  await signIn(b, '/auth/login', name);
  await expect(lists(b)).toHaveText([/Keep/, /Doomed/]);

  a.once('dialog', (dialog) => dialog.accept());
  await a.getByRole('button', { name: 'Delete list' }).click();
  await expect(lists(a)).toHaveText([/Keep/]);

  // Other devices find out when they next connect.
  await b.reload();
  await expect(lists(b)).toHaveText([/Keep/]);
  await a.reload();
  await expect(lists(a)).toHaveText([/Keep/]);
});

test('signing out erases the lists on this device only', async ({ browser }) => {
  const name = uniqueName('out');
  const page = await (await browser.newContext()).newPage();
  await signIn(page, invite(), name);
  await createList(page, 'Private');
  await page.goto('/');
  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page.getByText('Lists are stored on this device only.')).toBeVisible();
  await expect(lists(page)).toHaveCount(0);

  await signIn(page, '/auth/login', name);
  await expect(lists(page)).toHaveText([/Private/]);
});

test('a disabled account keeps its local lists but stops syncing', async ({ browser }) => {
  const name = uniqueName('gone');
  const page = await (await browser.newContext()).newPage();
  await signIn(page, invite(), name);
  await createList(page, 'Mine');
  await page.goto('/');
  await expect(page.locator('.sync-status')).toHaveText('Synced');

  admin('user', 'disable', userId(name));
  await page.reload();
  await expect(page.getByText(`Signed out of ${name}@dev.invalid`)).toBeVisible();
  await expect(lists(page)).toHaveText([/Mine/]);
});
