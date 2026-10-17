//! Server secrets, such as the cookie signing key.

use sqlx::SqlitePool;

#[derive(Clone)]
pub struct KeyStore {
    pool: SqlitePool,
}

impl KeyStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// A 64-byte secret stored under `name`, generated on first use. Keeping
    /// it in the database means restarts don't invalidate cookies and there
    /// is no key file to manage.
    pub async fn get(&self, name: &str) -> anyhow::Result<[u8; 64]> {
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
}
