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

# Accounts need an invite, from the admin CLI or a list's Share panel:
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
supp admin backup <file> [--force]
```

### Backups

`supp admin backup` writes a consistent copy of the database with SQLite's
`VACUUM INTO`, which is safe while the server runs. It won't replace an
existing file without `--force`, and writes to `<file>.partial` first, so a
failed run never clobbers the previous backup. For example, a daily cron job
keeping one copy per weekday (`%` must be escaped in a crontab):

```cron
17 3 * * * SUPP_DB=/var/lib/supp/supp.db /usr/local/bin/supp admin backup --force /var/backups/supp/supp-$(date +\%a).db
```

Copy the backups off the box too. They hold everything, including session
hashes and the server's cookie key, so keep them as private as the database.

To restore: stop the server, delete `supp.db-wal` and `supp.db-shm` next to
the database if they exist (they belong to the old file), copy the backup
over `supp.db`, and start the server again. List contents newer than the
backup come back as devices sync, since each keeps its own copy; accounts,
memberships, invites and sessions made since are lost, so some people may
have to sign in again.

### Deploying with Dokku

`Containerfile` builds a `scratch` image holding only the static `supp`
binary and the built client. It listens on port 5000, keeps the database at
`/data/supp.db` and runs as user `supp`. Its uid defaults to 32767, the
owner Dokku's `storage:ensure-directory` gives the directory; override it
with `--build-arg APP_UID=...` (`dokku docker-options:add supp build ...`). On the Dokku host:

```sh
dokku apps:create supp
dokku builder-dockerfile:set supp dockerfile-path Containerfile
dokku storage:ensure-directory supp
dokku storage:mount supp /var/lib/dokku/data/storage/supp:/data
dokku config:set supp SUPP_PUBLIC_URL=https://lists.example.com SUPP_AUTH=google \
  SUPP_GOOGLE_CLIENT_ID=... SUPP_GOOGLE_CLIENT_SECRET=...
dokku domains:set supp lists.example.com
dokku letsencrypt:enable supp   # dokku-letsencrypt plugin
```

Then `git push dokku main`. The image needs BuildKit, Docker's default
builder since 23.0. Admin commands run in a one-off container against the
same volume:

```sh
dokku run supp admin invite create
dokku run supp admin backup --force /data/backup-$(date +%a).db
```

The image has no shell, so `dokku enter` doesn't work; use `dokku run`.
