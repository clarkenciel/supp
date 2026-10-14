//! SQLite persistence for Automerge documents.

use std::{path::Path, str::FromStr, time::Duration};

use anyhow::Context;
use automerge::Automerge;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use supp_shared::DocId;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

/// A document as loaded from disk, with the number of change rows stacked on
/// its snapshot.
pub struct Loaded {
    pub doc: Automerge,
    pub change_rows: usize,
}

impl Store {
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite:")?
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // In WAL mode NORMAL can lose the latest commits on power loss but
            // never corrupts; clients still hold those changes and resend them.
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .with_context(|| format!("opening {}", path.display()))?;
        sqlx::migrate!().run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn accounts(&self) -> crate::accounts::Accounts {
        crate::accounts::Accounts::new(self.pool.clone())
    }

    /// A 64-byte secret stored under `name`, generated on first use. Keeping
    /// it in the database means restarts don't invalidate cookies and there
    /// is no key file to manage.
    pub async fn server_key(&self, name: &str) -> anyhow::Result<[u8; 64]> {
        let mut fresh = [0u8; 64];
        getrandom::fill(&mut fresh).expect("system randomness");
        sqlx::query("INSERT OR IGNORE INTO server_keys (name, key) VALUES (?, ?)")
            .bind(name)
            .bind(fresh.as_slice())
            .execute(&self.pool)
            .await?;
        let key: Vec<u8> = sqlx::query_scalar("SELECT key FROM server_keys WHERE name = ?")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        key.try_into()
            .map_err(|_| anyhow::anyhow!("server key {name} has the wrong length"))
    }

    pub async fn close(&self) {
        self.pool.close().await;
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
