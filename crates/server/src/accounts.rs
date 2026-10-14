//! Users, sessions, invites and list memberships.
//!
//! Accounts are only created by redeeming an invite. Invites and sessions are
//! bearer tokens; only their SHA-256 hashes are stored, so a copy of the
//! database contains no usable links or sessions.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, SqlitePool, Transaction};
use supp_shared::DocId;

pub const SESSION_TTL: Duration = Duration::from_secs(180 * 24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UserId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InviteId(pub i64);

/// An identity verified by an identity provider. `subject` is namespaced by
/// provider (`google:<sub>`), so it is stable and never an email address.
#[derive(Clone, Debug)]
pub struct Identity {
    pub subject: String,
    pub email: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    pub subject: String,
    pub email: String,
    pub name: String,
}

/// A secret bearer token: 32 random bytes, base64url.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("system randomness");
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn hash(&self) -> Vec<u8> {
        Sha256::digest(self.0.as_bytes()).to_vec()
    }
}

impl From<String> for Token {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

pub struct NewInvite {
    /// Set for a list share link; `None` for an account-only invite.
    pub doc_id: Option<DocId>,
    /// `None` when created with the admin CLI.
    pub created_by: Option<UserId>,
    pub note: String,
    pub ttl: Duration,
    pub max_uses: u32,
}

#[derive(Debug)]
pub struct InviteInfo {
    pub id: InviteId,
    pub doc_id: Option<DocId>,
    /// Email of the creating user; `None` for admin invites.
    pub created_by: Option<String>,
    pub note: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub max_uses: i64,
    pub uses: i64,
    pub revoked: bool,
}

#[derive(Debug)]
pub struct UserInfo {
    pub id: UserId,
    pub subject: String,
    pub email: String,
    pub name: String,
    /// Email of the user whose invite created this account; `None` for admin
    /// invites.
    pub invited_by: Option<String>,
    pub created_at: i64,
    pub disabled: bool,
}

#[derive(Debug)]
pub struct Login {
    pub user: User,
    /// The list joined through a share link, if any.
    pub joined: Option<DocId>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("an invite is required to create an account")]
    InviteRequired,
    #[error("this account is disabled")]
    Disabled,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Access {
    Member,
    /// The document had no members, so the user became its owner.
    Claimed,
    Denied,
}

#[derive(Clone)]
pub struct Accounts {
    pool: SqlitePool,
}

impl Accounts {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    // Invites

    pub async fn create_invite(&self, invite: NewInvite) -> sqlx::Result<(InviteId, Token)> {
        let token = Token::generate();
        let now = now();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO invites \
             (token_hash, doc_id, created_by, note, created_at, expires_at, max_uses) \
             VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(token.hash())
        .bind(invite.doc_id.map(|d| d.as_bytes().to_vec()))
        .bind(invite.created_by.map(|u| u.0))
        .bind(&invite.note)
        .bind(now)
        .bind(now.saturating_add(invite.ttl.as_secs() as i64))
        .bind(i64::from(invite.max_uses))
        .fetch_one(&self.pool)
        .await?;
        Ok((InviteId(id), token))
    }

    pub async fn list_invites(&self) -> sqlx::Result<Vec<InviteInfo>> {
        let rows: Vec<InviteRow> = sqlx::query_as(
            "SELECT i.id, i.doc_id, u.email AS created_by, i.note, i.created_at, \
             i.expires_at, i.max_uses, i.uses, i.revoked_at IS NOT NULL AS revoked \
             FROM invites i LEFT JOIN users u ON u.id = i.created_by ORDER BY i.id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| InviteInfo {
                id: InviteId(r.id),
                doc_id: r.doc_id.and_then(|b| DocId::from_bytes(&b).ok()),
                created_by: r.created_by,
                note: r.note,
                created_at: r.created_at,
                expires_at: r.expires_at,
                max_uses: r.max_uses,
                uses: r.uses,
                revoked: r.revoked,
            })
            .collect())
    }

    /// Returns whether an unrevoked invite was revoked.
    pub async fn revoke_invite(&self, id: InviteId) -> sqlx::Result<bool> {
        let result =
            sqlx::query("UPDATE invites SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
                .bind(now())
                .bind(id.0)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() == 1)
    }

    // Login

    /// Logs in a verified identity.
    ///
    /// - Unknown identity: redeems `invite` (either kind) and creates the
    ///   account, joining the invite's list if it is a share link.
    /// - Known identity: logs in; a share-link `invite` also joins its list.
    ///   Account-only invites are left unused.
    pub async fn login(
        &self,
        identity: &Identity,
        invite: Option<&Token>,
    ) -> Result<Login, LoginError> {
        let now = now();
        // IMMEDIATE takes the write lock up front, so the read-then-write
        // below cannot fail midway with SQLITE_BUSY.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let existing: Option<(i64, Option<i64>)> =
            sqlx::query_as("SELECT id, disabled_at FROM users WHERE subject = ?")
                .bind(&identity.subject)
                .fetch_optional(&mut *tx)
                .await?;

        let (id, joined) = match existing {
            Some((_, Some(_))) => return Err(LoginError::Disabled),
            Some((id, None)) => {
                sqlx::query("UPDATE users SET email = ?, name = ? WHERE id = ?")
                    .bind(&identity.email)
                    .bind(&identity.name)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                let joined = match invite {
                    Some(token) => redeem(&mut tx, token, now, true).await?.and_then(|r| r.1),
                    None => None,
                };
                (id, joined)
            }
            None => {
                let token = invite.ok_or(LoginError::InviteRequired)?;
                let (invite_id, joined) = redeem(&mut tx, token, now, false)
                    .await?
                    .ok_or(LoginError::InviteRequired)?;
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO users (subject, email, name, invite_id, created_at) \
                     VALUES (?, ?, ?, ?, ?) RETURNING id",
                )
                .bind(&identity.subject)
                .bind(&identity.email)
                .bind(&identity.name)
                .bind(invite_id)
                .bind(now)
                .fetch_one(&mut *tx)
                .await?;
                (id, joined)
            }
        };
        if let Some(doc) = joined {
            add_member(&mut tx, doc, UserId(id), "member", now).await?;
        }
        tx.commit().await?;

        Ok(Login {
            user: User {
                id: UserId(id),
                subject: identity.subject.clone(),
                email: identity.email.clone(),
                name: identity.name.clone(),
            },
            joined,
        })
    }

    // Sessions

    pub async fn create_session(&self, user: UserId) -> sqlx::Result<Token> {
        let token = Token::generate();
        let now = now();
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
        )
        .bind(token.hash())
        .bind(user.0)
        .bind(now)
        .bind(now + SESSION_TTL.as_secs() as i64)
        .execute(&self.pool)
        .await?;
        Ok(token)
    }

    /// The user behind a live session, if the session exists, has not
    /// expired, and the user is not disabled.
    pub async fn session_user(&self, token: &Token) -> sqlx::Result<Option<User>> {
        let row: Option<(i64, String, String, String)> = sqlx::query_as(
            "SELECT u.id, u.subject, u.email, u.name FROM sessions s \
             JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash = ? AND s.expires_at > ? AND u.disabled_at IS NULL",
        )
        .bind(token.hash())
        .bind(now())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, subject, email, name)| User {
            id: UserId(id),
            subject,
            email,
            name,
        }))
    }

    pub async fn delete_session(&self, token: &Token) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token.hash())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Users

    pub async fn is_active(&self, user: UserId) -> sqlx::Result<bool> {
        let active: Option<bool> =
            sqlx::query_scalar("SELECT disabled_at IS NULL FROM users WHERE id = ?")
                .bind(user.0)
                .fetch_optional(&self.pool)
                .await?;
        Ok(active.unwrap_or(false))
    }

    pub async fn list_users(&self) -> sqlx::Result<Vec<UserInfo>> {
        let rows: Vec<UserRow> = sqlx::query_as(
            "SELECT u.id, u.subject, u.email, u.name, inviter.email AS invited_by, \
             u.created_at, u.disabled_at IS NOT NULL AS disabled \
             FROM users u \
             LEFT JOIN invites i ON i.id = u.invite_id \
             LEFT JOIN users inviter ON inviter.id = i.created_by \
             ORDER BY u.id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| UserInfo {
                id: UserId(r.id),
                subject: r.subject,
                email: r.email,
                name: r.name,
                invited_by: r.invited_by,
                created_at: r.created_at,
                disabled: r.disabled,
            })
            .collect())
    }

    /// Disables a user, ends their sessions and revokes the invites they
    /// created. Returns whether an active user was disabled.
    pub async fn disable_user(&self, user: UserId) -> sqlx::Result<bool> {
        let now = now();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let disabled =
            sqlx::query("UPDATE users SET disabled_at = ? WHERE id = ? AND disabled_at IS NULL")
                .bind(now)
                .bind(user.0)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                == 1;
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(user.0)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE invites SET revoked_at = ? WHERE created_by = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(user.0)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(disabled)
    }

    // Memberships

    /// Whether `user` may sync `doc`. A document nobody belongs to yet is
    /// claimed by the first user to sync it; UUIDv4 IDs are unguessable, so
    /// only its creator can do that.
    pub async fn authorize(&self, user: UserId, doc: DocId) -> sqlx::Result<Access> {
        let doc_bytes = doc.as_bytes().as_slice();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let (members, is_member): (i64, bool) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(MAX(user_id = ?), 0) FROM list_members WHERE doc_id = ?",
        )
        .bind(user.0)
        .bind(doc_bytes)
        .fetch_one(&mut *tx)
        .await?;
        let access = if is_member {
            Access::Member
        } else if members > 0 {
            Access::Denied
        } else {
            add_member(&mut tx, doc, user, "owner", now()).await?;
            Access::Claimed
        };
        tx.commit().await?;
        Ok(access)
    }

    /// Removes `user` from the list. The document itself is kept.
    pub async fn leave(&self, user: UserId, doc: DocId) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM list_members WHERE doc_id = ? AND user_id = ?")
            .bind(doc.as_bytes().as_slice())
            .bind(user.0)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn docs_for(&self, user: UserId) -> sqlx::Result<Vec<DocId>> {
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT doc_id FROM list_members WHERE user_id = ? ORDER BY added_at",
        )
        .bind(user.0)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|b| DocId::from_bytes(b).ok())
            .collect())
    }
}

#[derive(sqlx::FromRow)]
struct InviteRow {
    id: i64,
    doc_id: Option<Vec<u8>>,
    created_by: Option<String>,
    note: String,
    created_at: i64,
    expires_at: i64,
    max_uses: i64,
    uses: i64,
    revoked: bool,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: i64,
    subject: String,
    email: String,
    name: String,
    invited_by: Option<String>,
    created_at: i64,
    disabled: bool,
}

/// Consumes one use of a valid invite, returning its ID and list. With
/// `share_only`, account-only invites are not consumed.
///
/// A single conditional UPDATE, so two people racing for the last use of an
/// invite cannot both succeed.
async fn redeem(
    tx: &mut Transaction<'_, Sqlite>,
    token: &Token,
    now: i64,
    share_only: bool,
) -> sqlx::Result<Option<(i64, Option<DocId>)>> {
    let row: Option<(i64, Option<Vec<u8>>)> = sqlx::query_as(
        "UPDATE invites SET uses = uses + 1 \
         WHERE token_hash = ? AND uses < max_uses AND expires_at > ? AND revoked_at IS NULL \
         AND (? = 0 OR doc_id IS NOT NULL) \
         RETURNING id, doc_id",
    )
    .bind(token.hash())
    .bind(now)
    .bind(share_only)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|(id, doc)| (id, doc.and_then(|b| DocId::from_bytes(&b).ok()))))
}

async fn add_member(
    tx: &mut Transaction<'_, Sqlite>,
    doc: DocId,
    user: UserId,
    role: &str,
    now: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO list_members (doc_id, user_id, role, added_at) VALUES (?, ?, ?, ?)",
    )
    .bind(doc.as_bytes().as_slice())
    .bind(user.0)
    .bind(role)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    async fn accounts() -> (tempfile::TempDir, Accounts) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("supp.db")).await.unwrap();
        (dir, store.accounts())
    }

    fn identity(name: &str) -> Identity {
        Identity {
            subject: format!("test:{name}"),
            email: format!("{name}@example.com"),
            name: name.into(),
        }
    }

    fn invite(doc_id: Option<DocId>, max_uses: u32) -> NewInvite {
        NewInvite {
            doc_id,
            created_by: None,
            note: String::new(),
            ttl: Duration::from_secs(3600),
            max_uses,
        }
    }

    async fn signup(accounts: &Accounts, name: &str) -> User {
        let (_, token) = accounts.create_invite(invite(None, 1)).await.unwrap();
        accounts
            .login(&identity(name), Some(&token))
            .await
            .unwrap()
            .user
    }

    #[tokio::test]
    async fn signup_requires_a_valid_invite() {
        let (_dir, accounts) = accounts().await;
        assert!(matches!(
            accounts.login(&identity("a"), None).await,
            Err(LoginError::InviteRequired)
        ));
        let bogus = Token::generate();
        assert!(matches!(
            accounts.login(&identity("a"), Some(&bogus)).await,
            Err(LoginError::InviteRequired)
        ));

        let expired = NewInvite {
            ttl: Duration::ZERO,
            ..invite(None, 1)
        };
        let (_, token) = accounts.create_invite(expired).await.unwrap();
        assert!(matches!(
            accounts.login(&identity("a"), Some(&token)).await,
            Err(LoginError::InviteRequired)
        ));

        let (id, token) = accounts.create_invite(invite(None, 1)).await.unwrap();
        assert!(accounts.revoke_invite(id).await.unwrap());
        assert!(matches!(
            accounts.login(&identity("a"), Some(&token)).await,
            Err(LoginError::InviteRequired)
        ));
    }

    #[tokio::test]
    async fn invite_uses_are_limited() {
        let (_dir, accounts) = accounts().await;
        let (_, token) = accounts.create_invite(invite(None, 2)).await.unwrap();
        accounts.login(&identity("a"), Some(&token)).await.unwrap();
        accounts.login(&identity("b"), Some(&token)).await.unwrap();
        assert!(matches!(
            accounts.login(&identity("c"), Some(&token)).await,
            Err(LoginError::InviteRequired)
        ));
    }

    #[tokio::test]
    async fn racing_for_the_last_use_admits_one() {
        let (_dir, accounts) = accounts().await;
        let (_, token) = accounts.create_invite(invite(None, 1)).await.unwrap();
        let attempts = (0..8).map(|i| {
            let accounts = accounts.clone();
            let token = token.clone();
            tokio::spawn(async move {
                accounts
                    .login(&identity(&format!("u{i}")), Some(&token))
                    .await
            })
        });
        let mut admitted = 0;
        for attempt in attempts {
            match attempt.await.unwrap() {
                Ok(_) => admitted += 1,
                Err(LoginError::InviteRequired) => {}
                Err(err) => panic!("unexpected error: {err}"),
            }
        }
        assert_eq!(admitted, 1);
    }

    #[tokio::test]
    async fn returning_users_need_no_invite_and_keep_account_invites_unused() {
        let (_dir, accounts) = accounts().await;
        let user = signup(&accounts, "a").await;

        let mut renamed = identity("a");
        renamed.email = "new@example.com".into();
        let (_, token) = accounts.create_invite(invite(None, 1)).await.unwrap();
        let login = accounts.login(&renamed, Some(&token)).await.unwrap();
        assert_eq!(login.user.id, user.id);
        assert_eq!(login.user.email, "new@example.com");

        // The account-only invite is still usable by someone new.
        accounts.login(&identity("b"), Some(&token)).await.unwrap();
    }

    #[tokio::test]
    async fn share_links_create_accounts_and_join_lists() {
        let (_dir, accounts) = accounts().await;
        let owner = signup(&accounts, "owner").await;
        let doc = DocId::new();
        assert_eq!(
            accounts.authorize(owner.id, doc).await.unwrap(),
            Access::Claimed
        );

        let share = NewInvite {
            created_by: Some(owner.id),
            ..invite(Some(doc), 2)
        };
        let (_, token) = accounts.create_invite(share).await.unwrap();

        let new = accounts
            .login(&identity("new"), Some(&token))
            .await
            .unwrap();
        assert_eq!(new.joined, Some(doc));
        assert_eq!(
            accounts.authorize(new.user.id, doc).await.unwrap(),
            Access::Member
        );

        let existing = signup(&accounts, "existing").await;
        assert_eq!(
            accounts.authorize(existing.id, doc).await.unwrap(),
            Access::Denied
        );
        let login = accounts
            .login(&identity("existing"), Some(&token))
            .await
            .unwrap();
        assert_eq!(login.joined, Some(doc));
        assert_eq!(accounts.docs_for(existing.id).await.unwrap(), vec![doc]);

        let users = accounts.list_users().await.unwrap();
        let new_info = users.iter().find(|u| u.id == new.user.id).unwrap();
        assert_eq!(new_info.invited_by.as_deref(), Some("owner@example.com"));
    }

    #[tokio::test]
    async fn sessions() {
        let (_dir, accounts) = accounts().await;
        let user = signup(&accounts, "a").await;
        let token = accounts.create_session(user.id).await.unwrap();
        assert_eq!(
            accounts.session_user(&token).await.unwrap(),
            Some(user.clone())
        );
        assert_eq!(
            accounts.session_user(&Token::generate()).await.unwrap(),
            None
        );
        accounts.delete_session(&token).await.unwrap();
        assert_eq!(accounts.session_user(&token).await.unwrap(), None);
    }

    #[tokio::test]
    async fn disabling_ends_sessions_blocks_login_and_revokes_invites() {
        let (_dir, accounts) = accounts().await;
        let user = signup(&accounts, "a").await;
        let session = accounts.create_session(user.id).await.unwrap();
        let share = NewInvite {
            created_by: Some(user.id),
            ..invite(Some(DocId::new()), 5)
        };
        let (_, token) = accounts.create_invite(share).await.unwrap();

        assert!(accounts.disable_user(user.id).await.unwrap());
        assert!(!accounts.is_active(user.id).await.unwrap());
        assert_eq!(accounts.session_user(&session).await.unwrap(), None);
        assert!(matches!(
            accounts.login(&identity("a"), None).await,
            Err(LoginError::Disabled)
        ));
        assert!(matches!(
            accounts.login(&identity("b"), Some(&token)).await,
            Err(LoginError::InviteRequired)
        ));
        assert!(accounts.list_invites().await.unwrap()[1].revoked);
    }

    #[tokio::test]
    async fn first_syncer_claims_a_document() {
        let (_dir, accounts) = accounts().await;
        let a = signup(&accounts, "a").await;
        let b = signup(&accounts, "b").await;
        let doc = DocId::new();
        assert_eq!(
            accounts.authorize(a.id, doc).await.unwrap(),
            Access::Claimed
        );
        assert_eq!(accounts.authorize(a.id, doc).await.unwrap(), Access::Member);
        assert_eq!(accounts.authorize(b.id, doc).await.unwrap(), Access::Denied);
        assert_eq!(accounts.docs_for(a.id).await.unwrap(), vec![doc]);
        assert!(accounts.docs_for(b.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn leaving_a_list() {
        let (_dir, accounts) = accounts().await;
        let a = signup(&accounts, "a").await;
        let doc = DocId::new();
        accounts.authorize(a.id, doc).await.unwrap();
        accounts.leave(a.id, doc).await.unwrap();
        accounts.leave(a.id, doc).await.unwrap();
        assert!(accounts.docs_for(a.id).await.unwrap().is_empty());
    }
}
