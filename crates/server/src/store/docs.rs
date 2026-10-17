//! Automerge documents: a snapshot plus the incremental changes since it.

use automerge::Automerge;
use sqlx::SqlitePool;
use supp_shared::DocId;

#[derive(Clone)]
pub struct DocStore {
    pool: SqlitePool,
}

/// A document as loaded from disk, with the number of change rows stacked on
/// its snapshot.
pub struct Loaded {
    pub doc: Automerge,
    pub change_rows: usize,
}

impl DocStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Loads a document, or `None` if nothing has been stored for it.
    pub async fn load(&self, id: DocId) -> anyhow::Result<Option<Loaded>> {
        let id = id.as_bytes().as_slice();
        let snapshot: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT bytes FROM doc_snapshots WHERE doc_id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        let changes: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT bytes FROM doc_changes WHERE doc_id = ? ORDER BY seq")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
        if snapshot.is_none() && changes.is_empty() {
            return Ok(None);
        }
        let mut doc = match snapshot {
            Some(bytes) => Automerge::load(&bytes)?,
            None => Automerge::new(),
        };
        for bytes in &changes {
            doc.load_incremental(bytes)?;
        }
        Ok(Some(Loaded {
            doc,
            change_rows: changes.len(),
        }))
    }

    /// Appends incremental changes (from `Automerge::save_after`).
    pub async fn append(&self, id: DocId, bytes: &[u8]) -> anyhow::Result<()> {
        let id = id.as_bytes().as_slice();
        sqlx::query(
            "INSERT INTO doc_changes (doc_id, seq, bytes) VALUES (?1, \
             COALESCE((SELECT MAX(seq) FROM doc_changes WHERE doc_id = ?1), 0) + 1, ?2)",
        )
        .bind(id)
        .bind(bytes)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replaces the snapshot with a full save and drops the change rows.
    pub async fn compact(&self, id: DocId, snapshot: &[u8]) -> anyhow::Result<()> {
        let id = id.as_bytes().as_slice();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO doc_snapshots (doc_id, bytes) VALUES (?1, ?2) \
             ON CONFLICT (doc_id) DO UPDATE SET bytes = excluded.bytes",
        )
        .bind(id)
        .bind(snapshot)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM doc_changes WHERE doc_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn change_rows(&self, id: DocId) -> anyhow::Result<usize> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM doc_changes WHERE doc_id = ?")
            .bind(id.as_bytes().as_slice())
            .fetch_one(&self.pool)
            .await?;
        Ok(n as usize)
    }
}
