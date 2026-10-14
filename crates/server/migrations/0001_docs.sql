-- Automerge documents. A document is its snapshot (if any) followed by its
-- incremental change rows in seq order.
CREATE TABLE doc_snapshots (
    doc_id BLOB PRIMARY KEY,
    bytes  BLOB NOT NULL
) STRICT;

CREATE TABLE doc_changes (
    doc_id BLOB NOT NULL,
    seq    INTEGER NOT NULL,
    bytes  BLOB NOT NULL,
    PRIMARY KEY (doc_id, seq)
) STRICT;
