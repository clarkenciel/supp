// Shared helpers for signed-in tests.
const { execFileSync } = require('node:child_process');
const path = require('node:path');
const { expect } = require('@playwright/test');

const root = path.join(__dirname, '..', '..');
const supp = path.join(root, 'target', 'debug', 'supp');
const db = path.join(root, 'e2e', '.data', 'supp.db');
const publicUrl = 'http://127.0.0.1:3100';

function admin(...args) {
  return execFileSync(supp, ['--db', db, 'admin', ...args], { encoding: 'utf8' });
}

/** A fresh account invite link from the admin CLI. */
function invite() {
  return admin('invite', 'create', '--public-url', publicUrl).trim().split('\n').pop();
}

/** The user ID `admin user list` shows for a dev-provider name. */
function userId(name) {
  const line = admin('user', 'list')
    .split('\n')
    .find((l) => l.includes(`<${name}@dev.invalid>`));
  return line.trim().split(/\s+/)[0];
}

/** A name no other test uses (the database lives for the whole run). */
function uniqueName(prefix) {
  return `${prefix}${Date.now()}${Math.floor(Math.random() * 1000)}`;
}

/** Signs in through the dev provider, starting at `url`. */
async function signIn(page, url, name) {
  await page.goto(url);
  await page.getByLabel('Name').fill(name);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.locator('.sync-status')).toHaveText('Synced');
}

const items = (page) => page.locator('ul.items li input.text');

async function expectItems(page, expected) {
  await expect
    .poll(() => items(page).evaluateAll((els) => els.map((el) => el.value)))
    .toEqual(expected);
}

async function createList(page, title) {
  await page.goto('/');
  await page.getByLabel('New list title').fill(title);
  await page.getByRole('button', { name: 'Create' }).click();
  await expect(page.getByLabel('List title')).toHaveValue(title);
}

async function addItem(page, text) {
  await page.getByLabel('New item').fill(text);
  await page.getByLabel('New item').press('Enter');
  await expect(items(page).last()).toHaveValue(text);
}

/**
 * Cuts the network. setOffline alone doesn't reach the service worker's own
 * fetches, so those are aborted by routing (which needs
 * PW_EXPERIMENTAL_SERVICE_WORKER_NETWORK_EVENTS, set in playwright.config.js).
 */
async function goOffline(context) {
  await context.setOffline(true);
  await context.route('**', (route) => route.abort('internetdisconnected'));
}

module.exports = {
  admin, invite, userId, uniqueName, signIn, items, expectItems, createList, addItem, goOffline,
};
