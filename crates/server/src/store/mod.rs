//! The SQLite database. `Store` owns the connection pool and hands out a
//! store for each kind of data in it.

mod docs;
mod keys;

use std::{path::Path, str::FromStr, time::Duration};

use anyhow::Context;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

pub use docs::{DocStore, Loaded};
pub use keys::KeyStore;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
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

    pub fn docs(&self) -> DocStore {
        DocStore::new(self.pool.clone())
    }

    pub fn accounts(&self) -> crate::accounts::Accounts {
        crate::accounts::Accounts::new(self.pool.clone())
    }

    pub fn keys(&self) -> KeyStore {
        KeyStore::new(self.pool.clone())
    }

    /// Writes a consistent copy of the whole database to `path`, which must
    /// not exist. `VACUUM INTO` reads one snapshot of the database, so it is
    /// safe while the server writes (WAL), and the copy is compact.
    pub async fn backup(&self, path: &Path) -> anyhow::Result<()> {
        let path = path
            .to_str()
            .context("backup path must be valid UTF-8 for SQLite")?;
        sqlx::query("VACUUM INTO ?")
            .bind(path)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}
