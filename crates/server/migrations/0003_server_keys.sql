-- Server-generated secrets, created on first use (e.g. the cookie key).
CREATE TABLE server_keys (
    name TEXT PRIMARY KEY,
    key  BLOB NOT NULL
) STRICT;
