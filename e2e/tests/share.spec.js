// Sharing lists by link: signing up and joining through one, and links
// that no longer work. Each browser context is a separate person.
const { test, expect } = require('@playwright/test');
const {
  invite, uniqueName, signIn, expectItems, createList, addItem, goOffline,
} = require('./helpers');

const sharePanel = (page) => page.getByRole('region', { name: 'Share this list' });
const shareButton = (page) => page.getByRole('button', { name: 'Share', exact: true });

/** Creates a share link for the open list, opening the share panel if needed. */
async function shareLink(page, uses) {
  const panel = sharePanel(page);
  if (!(await panel.isVisible())) await shareButton(page).click();
  if (uses) await panel.getByLabel('Can be used by').selectOption(uses);
  await panel.getByRole('button', { name: 'Create link' }).click();
  const link = panel.getByLabel('Share link');
  await expect(link).toHaveValue(/^http:\/\/127\.0\.0\.1:3100\/join\/[\w-]{43}$/);
  return link.inputValue();
}

/** Closes and reopens the share panel, which reloads its links. */
async function reopenSharePanel(page) {
  await shareButton(page).click();
  await expect(sharePanel(page)).toBeHidden();
  await shareButton(page).click();
}

/** Tries to sign up through `link` as someone new, and expects to be turned away. */
async function expectRefused(browser, link) {
  const page = await (await browser.newContext()).newPage();
  await page.goto(link);
  await page.getByLabel('Name').fill(uniqueName('turnedaway'));
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByText('You need an invite')).toBeVisible();
}

test('a new user signs up through a share link and edits with the owner', async ({ browser }) => {
  const owner = await (
    await browser.newContext({ permissions: ['clipboard-read', 'clipboard-write'] })
  ).newPage();
  await signIn(owner, invite(), uniqueName('owner'));
  await createList(owner, 'Party');
  await addItem(owner, 'cake');
  const link = await shareLink(owner);
  await expect(sharePanel(owner).getByRole('listitem')).toHaveText([
    /Made by you · not used yet · expires in 7 days/,
  ]);
  await sharePanel(owner).getByRole('button', { name: 'Copy' }).click();
  await expect(sharePanel(owner).getByText('Copied')).toBeVisible();
  expect(await owner.evaluate(() => navigator.clipboard.readText())).toBe(link);

  // No admin invite: the share link is enough to create an account, and
  // the sign-in ends on the list, which syncs in.
  const guest = await (await browser.newContext()).newPage();
  await signIn(guest, link, uniqueName('guest'));
  await expect(guest).toHaveURL(owner.url());
  await expect(guest.getByLabel('List title')).toHaveValue('Party');
  await expectItems(guest, ['cake']);

  await addItem(guest, 'balloons');
  await expectItems(owner, ['cake', 'balloons']);
  await owner.getByLabel('Done').first().check();
  await expect(guest.getByLabel('Done').first()).toBeChecked();

  await reopenSharePanel(owner);
  await expect(sharePanel(owner).getByRole('listitem')).toHaveText([/used up/]);
  await guest.goto('/');
  await expect(guest.locator('ul.lists li')).toHaveText([/Party/]);
});

test('an existing user joins through a share link', async ({ browser }) => {
  const owner = await (await browser.newContext()).newPage();
  await signIn(owner, invite(), uniqueName('owner'));
  await createList(owner, 'Shared');
  await addItem(owner, 'bread');
  const link = await shareLink(owner);

  const friendName = uniqueName('friend');
  const friend = await (await browser.newContext()).newPage();
  await signIn(friend, invite(), friendName);
  await createList(friend, 'Own');
  await signIn(friend, link, friendName);
  await expect(friend).toHaveURL(owner.url());
  await expectItems(friend, ['bread']);
  await friend.goto('/');
  await expect(friend.locator('ul.lists li')).toHaveText([/Own/, /Shared/]);
});

test('used-up and revoked links are refused', async ({ browser }) => {
  const owner = await (await browser.newContext()).newPage();
  await signIn(owner, invite(), uniqueName('owner'));
  await createList(owner, 'Secret');
  const single = await shareLink(owner);
  const first = await (await browser.newContext()).newPage();
  await signIn(first, single, uniqueName('first'));
  await expectItems(first, []);
  await expect(first.getByLabel('List title')).toHaveValue('Secret');
  await expectRefused(browser, single);

  const revoked = await shareLink(owner, '5');
  const links = sharePanel(owner).getByRole('listitem');
  await expect(links).toHaveText([/used up/, /not used yet/]);
  await links.filter({ hasText: 'not used yet' }).getByRole('button', { name: 'Revoke' }).click();
  await expect(links).toHaveText([/used up/]);
  await expectRefused(browser, revoked);
});

test('sharing asks anonymous users to sign in', async ({ page }) => {
  await createList(page, 'Mine');
  await shareButton(page).click();
  await expect(sharePanel(page)).toContainText('Sharing needs an account');
  await expect(page.getByRole('link', { name: 'Sign in to share' })).toHaveAttribute(
    'href',
    '/auth/login',
  );
});

test('sharing offline says it needs a connection', async ({ page, context }) => {
  await signIn(page, invite(), uniqueName('offline'));
  await createList(page, 'Trip');
  await shareButton(page).click();
  await expect(sharePanel(page)).toContainText('No active links.');
  await goOffline(context);
  await sharePanel(page).getByRole('button', { name: 'Create link' }).click();
  await expect(sharePanel(page).getByRole('alert')).toHaveText(/You're offline/);
});
