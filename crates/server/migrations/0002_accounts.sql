-- Accounts are created only by redeeming an invite. Times are unix seconds.

CREATE TABLE invites (
    id         INTEGER PRIMARY KEY,
    token_hash BLOB NOT NULL UNIQUE,        -- SHA-256 of the secret token
    doc_id     BLOB,                        -- NULL: account-only; set: share link
    created_by INTEGER REFERENCES users(id),-- NULL: admin CLI
    note       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    max_uses   INTEGER NOT NULL,
    uses       INTEGER NOT NULL DEFAULT 0,
    revoked_at INTEGER
) STRICT;

CREATE TABLE users (
    id          INTEGER PRIMARY KEY,
    subject     TEXT NOT NULL UNIQUE,       -- "<provider>:<provider subject>"
    email       TEXT NOT NULL,
    name        TEXT NOT NULL,
    invite_id   INTEGER REFERENCES invites(id),
    created_at  INTEGER NOT NULL,
    disabled_at INTEGER
) STRICT;

CREATE TABLE sessions (
    token_hash BLOB PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE list_members (
    doc_id   BLOB NOT NULL,
    user_id  INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role     TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    added_at INTEGER NOT NULL,
    PRIMARY KEY (doc_id, user_id)
) STRICT;

CREATE INDEX list_members_by_user ON list_members (user_id);
