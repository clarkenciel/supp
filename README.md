# supp

Offline-first collaborative lists. A Leptos PWA syncing Automerge documents
through an axum server backed by SQLite. See [docs/design.md](docs/design.md).

## Layout

- `proto/` — wire protocol and API bodies (protobuf)
- `crates/shared` — protocol types, document schema, IDs
- `crates/server` — `supp` binary: sync server and admin CLI
- `crates/web` — Leptos client (client-side rendered, built with trunk)
- `e2e/` — Playwright browser tests

## Development

Requires a stable Rust toolchain with the `wasm32-unknown-unknown` target,
[trunk](https://trunkrs.dev), and Node for the browser tests.

```sh
cargo test                                   # shared + server tests

# Server with the dev identity provider (anyone signs in as any name):
cargo run -p supp-server -- serve --auth dev --public-url http://127.0.0.1:8080
# Client with live reload; proxies /api, /auth, /join and /sync to the server.
# Its service worker caches nothing, so reloads always get the latest build:
(cd crates/web && trunk serve)               # http://127.0.0.1:8080

# Accounts need an invite:
cargo run -p supp-server -- admin invite create --public-url http://127.0.0.1:8080

(cd e2e && npm install && npx playwright test)   # builds and runs everything

python3 crates/web/scripts/make-icons.py     # redraw the app icons (Pillow)
```

trunk builds need a POSIX shell: a post-build hook stamps the service worker
with the build's version and file list.

## Running it for real

```sh
(cd crates/web && trunk build --release)
cargo build --release -p supp-server

SUPP_DB=/var/lib/supp/supp.db \
SUPP_PUBLIC_URL=https://lists.example.com \
SUPP_AUTH=google \
SUPP_GOOGLE_CLIENT_ID=... \
SUPP_GOOGLE_CLIENT_SECRET=... \
SUPP_STATIC_DIR=crates/web/dist \
  target/release/supp serve
```

Put it behind a TLS-terminating proxy (e.g. Caddy). In the Google Cloud
console, create an OAuth client of type "Web application" with the
redirect URI `https://lists.example.com/auth/callback`.

Admin, over SSH on the box (same `SUPP_DB`):

```sh
supp admin invite create [--expires 7d] [--uses 1] [--note "for Sam"]
supp admin invite list | revoke <id>
supp admin user list | disable <id>
```
