// supp service worker: keeps the app shell in a cache so the app starts
// with no network. Data lives in IndexedDB; server routes are never cached.
//
// scripts/sw-version.sh fills in VERSION and ASSETS after every trunk
// build, so each build's worker differs byte for byte (which is how
// browsers notice an update) and precaches exactly that build's files.
const VERSION = 'dev';
const ASSETS = [];

const PREFIX = 'supp-shell-';
const CACHE = PREFIX + VERSION;
const SERVER_ROUTE = /^\/(api|auth|join|sync)(\/|$)/;

self.addEventListener('install', (event) => {
  // Revalidate rather than trust the HTTP cache, so an unhashed file
  // (manifest, icons, index.html) is never precached stale.
  const requests = ASSETS.map((url) => new Request(url, { cache: 'no-cache' }));
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(requests)));
  // No skipWaiting here: a new version waits until the page asks for it
  // (see 'message'), so a running app never mixes two builds.
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(
        keys.filter((k) => k.startsWith(PREFIX) && k !== CACHE).map((k) => caches.delete(k)),
      ),
    ),
  );
});

self.addEventListener('message', (event) => {
  if (event.data === 'skipWaiting') self.skipWaiting();
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== 'GET' || url.origin !== self.location.origin) return;
  if (SERVER_ROUTE.test(url.pathname)) return;
  event.respondWith(
    caches.open(CACHE).then(async (cache) => {
      // Every client route is the same shell: cache first, so the app
      // starts at once even on a bad connection. Updates arrive as a new
      // worker, which the app offers to switch to.
      const cached = await cache.match(request.mode === 'navigate' ? 'index.html' : request);
      return cached || fetch(request);
    }),
  );
});
