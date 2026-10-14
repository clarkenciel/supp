# supp — design

An offline-first, collaborative list app. Progressive web app, Rust end to end.

## Goals and constraints

- **Scale:** a handful of users on a single server box. No pub/sub system, no
  database server: one axum process, in-process fan-out, SQLite.
- **Offline first:** the app is fully usable with no network and no account.
- **Collaboration:** lists sync between users and devices via Automerge.
- **Flat lists only.** Item text is last-writer-wins (no character-level merge).

## Stack

| Concern            | Choice                                                       |
|--------------------|--------------------------------------------------------------|
| Frontend           | Leptos 0.8, client-side rendering only, built with trunk     |
| Backend            | axum 0.8                                                     |
| CRDT / sync        | automerge 0.12 (`automerge::sync`), autosurgeon for structs  |
| Wire format        | protobuf via prost 0.14; protox compiles `.proto` (no protoc)|
| Server persistence | SQLite (WAL) via sqlx                                        |
| Client persistence | IndexedDB                                                    |
| Identity           | Google OIDC (openidconnect 4.x), invite-gated                |

Client-side rendering rather than server-side: the app shell is served from the
service worker cache and data comes from IndexedDB, so server rendering adds
hydration complexity without helping offline use.

samod (the Rust automerge-repo port) was considered and rejected for now; its
README describes it as a work in progress not for serious use. The raw
`automerge::sync` API is small enough to wrap ourselves.

## Workspace layout

```
proto/supp/sync/v1/sync.proto   # wire protocol
crates/
  shared/   # generated proto types, document schema, IDs, sync helpers
  server/   # axum: /sync (WebSocket), /api, /auth, /join, static files; admin CLI
  web/      # leptos CSR app → wasm
```

## Document model

One Automerge document per list.

```rust
struct ListDoc { schema_version: u32, title: String, items: HashMap<ItemId, Item> }
struct Item    { text: String, done: bool, position: String /* fractional index */ }
```

- Items live in a map ordered by a fractional-index `position`, not an Automerge
  list: moving an element of an Automerge list is delete + insert, so concurrent
  moves would duplicate items.
- Scalar fields are Automerge registers: concurrent writes converge on the same
  deterministic winner on every peer.
- `schema_version` versions the document contents; protobuf only versions the
  envelope. Hydration fills defaults for missing fields so fields can be added.
- Document IDs are UUIDv4s generated on the client, so lists can be created
  offline.
- **Actor IDs are random per tab/session, never per user.** Two live peers
  sharing an actor ID corrupt history.

## Wire protocol

One WebSocket per tab, binary protobuf frames, multiplexing many documents.

```proto
message ClientMessage {
  oneof msg { Hello hello = 1; Subscribe subscribe = 2; SyncPayload sync = 3;
              Unsubscribe unsubscribe = 4; Leave leave = 5; }
}
message ServerMessage {
  oneof msg { Welcome welcome = 1; SyncPayload sync = 2; Denied denied = 3; Error error = 4;
              UpgradeRequired upgrade = 5; SessionEnded session_ended = 6; Left left = 7; }
}
message Welcome     { uint32 protocol_version = 1; repeated bytes doc_ids = 2; } // the user's lists
message SyncPayload { bytes doc_id = 1; bytes data = 2; } // data = opaque automerge sync message
message Denied      { bytes doc_id = 1; string reason = 2; bool left = 3; }
```

- The upgrade requires a session (401 otherwise). `Welcome` lists the user's
  lists so a new device can add the ones it lacks.
- `SessionEnded` is sent before closing when the session dies (logout,
  expiry, disabled account), so the client can tell "sign in again" from a
  network drop.
- `Leave` removes the list from the user's account; `Left` confirms it.

- `Hello { protocol_version }` / `UpgradeRequired` let the server reject clients
  that a service worker has kept alive past their supported version, which
  triggers the client's "reload to update" prompt.
- Evolution rules: never reuse a field number, `reserved` removed ones, and
  ignore unknown `oneof` variants (prost decodes them as `None`).
- Sync state is per connection, in memory, reset on reconnect. That costs one
  extra round trip per document per reconnect, which is fine at this scale.
- The small REST API is defined in protobuf too (`api.proto`), so everything
  evolves the same way. OIDC redirects stay plain HTTP.

## Server

- Registry `HashMap<DocId, Arc<DocActor>>`; each actor holds a
  `Mutex<Automerge>` and a `watch::Sender` of the document heads.
- Receive path: `receive_sync_message` → if heads changed, append to SQLite,
  then publish the new heads → every other connection subscribed to that
  document calls `generate_sync_message` and sends the result.
- Persist before publishing. If the server dies before the write, the client
  still has the changes and resends them on the next sync.
- Authorization: membership checked on `Subscribe`. The first sync of a
  document that has never had members makes the sender its owner (UUIDv4s are
  unguessable). Leaving soft-deletes the membership (`left_at`), so a list
  someone left can't be claimed back by a stale copy on another device; that
  device gets `Denied { left: true }` and drops its copy. Automerge has no
  per-change permissions, so any member can edit anything in a list.

### SQLite schema

```sql
users(id, subject UNIQUE, email, name, invite_id, created_at, disabled_at)
sessions(token_hash, user_id, expires_at)
invites(id, token_hash UNIQUE,
        doc_id NULL,       -- NULL: account-only invite; set: share link for that list
        created_by NULL,   -- NULL: admin CLI; else user id
        created_at, expires_at, max_uses, uses, revoked_at)
list_members(doc_id, user_id, role, added_at, left_at)
doc_snapshots(doc_id PK, bytes)
doc_changes(doc_id, seq, bytes)
server_keys(name PK, key)  -- e.g. the login-cookie key, generated on first use
```

- Load = snapshot + `load_incremental` of the change rows.
- Compaction: after N change rows, write a fresh `save()` snapshot and delete
  the rows in one transaction.
- Backups: `VACUUM INTO` from cron.

### Deployment

One binary serves static assets, `/api`, `/auth`, `/join` and `/sync`, behind
Caddy for TLS (service workers require a secure context).

## Identity and access

### Anonymous use

With no account, everything works locally in IndexedDB and no WebSocket is
opened. Sharing prompts sign-in.

### Identity providers

Auth routes depend only on an `IdentityProvider` trait: `begin` returns where
to send the browser plus opaque provider state (kept in an encrypted cookie
until the callback), and `complete` turns the callback into a verified
identity. The routes own CSRF `state` checking. Users are keyed on a
provider-namespaced subject (`google:<sub>`), never on email.

- **Google** (production): OIDC authorization code flow with PKCE.
- **Dev** (`--auth dev`): a form at `/auth/dev` that signs anyone in as
  `dev:<name>`. For local development and tests only.

### Login (server-side authorization code flow + PKCE)

1. `GET /auth/login` stores state, nonce and PKCE verifier in a short-lived
   signed cookie and redirects to Google.
2. `GET /auth/callback` exchanges the code and verifies the ID token (issuer,
   audience, expiry, nonce, signature).
3. Users are keyed on the provider-namespaced subject, never email.
4. The server issues its own session: an `HttpOnly; Secure; SameSite=Lax`
   cookie (`Lax` because the callback is a top-level redirect from Google).
5. Google tokens are discarded; Google is used only to prove identity.

### Invites

Accounts can only be created through an invite. There are two kinds, in one
table:

- **Account invites**, created with the admin CLI (`doc_id` NULL).
- **List share links**, created by any member of a list (`doc_id` set). These
  also create an account if the recipient doesn't have one. Defaults: single
  use, 7-day expiry; the share dialog can raise either; links are revocable.

Tokens are 32 random bytes, base64url; only the SHA-256 hash is stored.

`/join/:token` places the token in the signed login cookie and starts login. In
the callback:

- **Unknown `sub`:** in one transaction, redeem the invite
  (`UPDATE invites SET uses = uses + 1 WHERE token_hash = ? AND uses < max_uses
  AND expires_at > now AND revoked_at IS NULL`, must touch exactly one row),
  create the user, and add a `list_members` row if `doc_id` is set. Otherwise
  403 "invite required".
- **Known `sub`:** log in; if the invite has a `doc_id`, add the membership.

`users.invite_id` records who let whom in.

### Admin CLI

Subcommands of the server binary, run over SSH; they write directly to the
SQLite file (WAL permits this while the server runs). No admin HTTP surface.

```
supp serve
supp admin invite create [--expires 7d] [--uses 1] [--note "..."]   # prints the link once
supp admin invite list                                              # both kinds, with creator and list
supp admin invite revoke <id>
supp admin user list                                                # includes inviting user
supp admin user disable <id>   # deletes sessions, revokes their outstanding invites
```

The server checks `disabled_at` on every session lookup, re-checks on each
`Subscribe`, and periodically re-validates open sockets.

### Session state on the client

IndexedDB records the account (`Me`) the local lists belong to. The app renders
from it immediately, then confirms with `GET /api/me`:

| Stored account | `/api/me`        | State                                          |
|----------------|------------------|------------------------------------------------|
| none           | 401              | Anonymous: local only                          |
| none           | user             | First sign-in: adopt; local lists upload       |
| A              | A                | Signed in                                      |
| A              | 401              | Expired: keep editing, offer sign-in           |
| A              | B                | Mismatch: add local lists to B, or erase them  |
| any            | unreachable      | Trust the stored account                       |

- Local lists upload by subscribing to them: the server sees unclaimed
  documents and makes the user their owner.
- A failed WebSocket upgrade looks the same as a network error from JS, so
  after one the client asks `/api/me`; a 401 means Expired.
- Sign-out needs the network: it ends the server session, then erases
  IndexedDB. Erasing locally while the session lived on would leave the
  browser signed in.

### Leaving lists

Deleting a list writes a tombstone (IndexedDB `left` store) in the same
transaction that removes its data. The sync loop sends `Leave` for each
tombstone after every `Welcome` (and at once if connected), and drops it only
on `Left`. So a delete made offline still reaches the account, and the next
`Welcome` can't bring the list back meanwhile. Other devices of the same user
find out on their next connection.

## Client

- IndexedDB stores: `docs` (local lists), `snapshots`, `changes` (append-only
  chunks keyed `[doc id, random id]`, so tabs append without coordinating),
  `meta` (the account), `left` (pending leaves). A new list's record and
  first snapshot are written in one transaction.
- After any change, hydrate `ListDoc` with autosurgeon into a `RwSignal`; render
  with keyed `<For>`. Whole-document hydration is fine at list sizes.
- Sync loop reconnects with backoff, driven by WebSocket state rather than
  `navigator.onLine`.
- Multiple tabs: each tab is its own peer with its own actor ID. Change chunks
  have random keys so writes never conflict; a `BroadcastChannel` tells other tabs
  to load new changes, which keeps tabs consistent while offline.
- Service worker: hand-written JS, cache-first for trunk's hashed assets, with a
  "new version, reload" prompt.
- Call `navigator.storage.persist()` to reduce eviction risk for non-installed
  sites.
- `getrandom` needs its `wasm_js` backend configured for wasm builds.

## Milestones

1. ✅ `shared` (proto + schema) and the server sync endpoint with SQLite
   storage and compaction. Convergence tested with two in-process peers.
2. ✅ Leptos UI with IndexedDB, anonymous and fully offline.
3. ✅ Google login (behind an identity-provider trait, with a dev provider),
   invites, admin CLI, local-list upload, WebSocket sync loop, leaving lists.
4. PWA manifest, service worker, `UpgradeRequired` flow.
5. List share links, multi-tab `BroadcastChannel`, backups.

## Risks and known gaps

- **Google code exchange is untested** against real credentials; the
  authorization URL is tested against Google's live discovery document and
  the surrounding routes with the dev provider.
- **Typing over a remote edit:** item inputs re-render from the document, so
  a remote change to the item being typed in replaces the unsaved text.
- **Leaving is not live:** other devices of the same user drop a left list
  on their next connection.

- **Wasm bundle size** with automerge included. Measured at the end of
  milestone 2 (`trunk build --release`, wasm-opt `-Oz`): 2.37 MB raw / 880 KB
  gzip with default release settings; 1.87 MB / 736 KB with LTO, one codegen
  unit and `opt-level = "z"` for the web crate. Acceptable for an app the
  service worker caches once per version; revisit if it grows.
- **Document growth:** deletions leave tombstones. Acceptable at this scale.
- **OIDC inside an installed iOS PWA:** redirects to `accounts.google.com` and
  back have historically had cookie and window-handoff quirks. Test on a real
  device in milestone 3.
