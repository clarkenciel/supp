# supp

Offline-first collaborative lists. A Leptos PWA syncing Automerge documents
through an axum server backed by SQLite. See [docs/design.md](docs/design.md).

## Layout

- `proto/` — wire protocol (protobuf)
- `crates/shared` — protocol types, document schema, IDs
- `crates/server` — `supp` binary: sync server (and, later, admin CLI)
- `crates/web` — Leptos client (client-side rendered, built with trunk)
- `e2e/` — Playwright browser tests

## Development

Requires a stable Rust toolchain with the `wasm32-unknown-unknown` target,
[trunk](https://trunkrs.dev), and Node for the browser tests.

```sh
cargo test                                  # shared + server tests
cargo run -p supp-server -- serve           # sync server on 127.0.0.1:3000
(cd crates/web && trunk serve)              # web client on 127.0.0.1:8080
(cd e2e && npm install && npx playwright test)
```

The web client does not talk to the server yet (milestone 3).
