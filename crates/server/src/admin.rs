//! `supp admin …`: invite and user management, run over SSH on the server
//! box. Writes go straight to SQLite (WAL permits this while the server
//! runs), so there is no admin HTTP surface to secure.

use std::{
    io::Write,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};

use crate::accounts::{Accounts, InviteId, NewInvite, UserId};

/// Parses durations like `30m`, `12h`, `7d`.
pub fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    let (number, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = number
        .parse()
        .with_context(|| format!("invalid duration {s:?}"))?;
    let secs = match unit {
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => bail!("invalid duration {s:?}: use a number followed by m, h or d"),
    };
    Ok(Duration::from_secs(n * secs))
}

pub fn join_url(public_url: &str, token: &str) -> String {
    format!("{}/join/{token}", public_url.trim_end_matches('/'))
}

pub async fn invite_create(
    accounts: &Accounts,
    public_url: &str,
    ttl: Duration,
    max_uses: u32,
    note: String,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let invite = NewInvite {
        doc_id: None,
        created_by: None,
        note,
        ttl,
        max_uses,
    };
    let (id, token) = accounts.create_invite(invite).await?;
    writeln!(out, "Invite {} (shown once):", id.0)?;
    writeln!(out, "{}", join_url(public_url, token.as_str()))?;
    Ok(())
}

pub async fn invite_list(accounts: &Accounts, out: &mut impl Write) -> anyhow::Result<()> {
    let now = unix_now();
    for i in accounts.list_invites().await? {
        let status = if i.revoked {
            "revoked".to_string()
        } else if i.uses >= i.max_uses {
            "used up".to_string()
        } else if i.expires_at <= now {
            "expired".to_string()
        } else {
            format!("expires in {}", human(i.expires_at - now))
        };
        let kind = match i.doc_id {
            Some(doc) => format!("share link for list {doc}"),
            None => "account invite".to_string(),
        };
        let creator = i.created_by.as_deref().unwrap_or("admin");
        writeln!(
            out,
            "{:>4}  {kind}  by {creator}  uses {}/{}  {status}  {}",
            i.id.0, i.uses, i.max_uses, i.note
        )?;
    }
    Ok(())
}

pub async fn invite_revoke(
    accounts: &Accounts,
    id: i64,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    if accounts.revoke_invite(InviteId(id)).await? {
        writeln!(out, "Revoked invite {id}.")?;
        Ok(())
    } else {
        bail!("no unrevoked invite {id}")
    }
}

pub async fn user_list(accounts: &Accounts, out: &mut impl Write) -> anyhow::Result<()> {
    for u in accounts.list_users().await? {
        let invited_by = u.invited_by.as_deref().unwrap_or("admin");
        let disabled = if u.disabled { "  DISABLED" } else { "" };
        writeln!(
            out,
            "{:>4}  {} <{}>  {}  invited by {invited_by}{disabled}",
            u.id.0, u.name, u.email, u.subject
        )?;
    }
    Ok(())
}

pub async fn user_disable(
    accounts: &Accounts,
    id: i64,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    if accounts.disable_user(UserId(id)).await? {
        writeln!(
            out,
            "Disabled user {id}; their sessions and invites are revoked."
        )?;
        Ok(())
    } else {
        bail!("no active user {id}")
    }
}

/// Rounds up, so a fresh 7-day invite reads "7d" rather than "6d".
fn human(secs: i64) -> String {
    let ceil = |unit: i64| (secs + unit - 1) / unit;
    match secs {
        s if s > 86_400 => format!("{}d", ceil(86_400)),
        s if s > 3_600 => format!("{}h", ceil(3_600)),
        _ => format!("{}m", ceil(60).max(1)),
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Store,
        accounts::{Identity, Token},
    };

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("12h").unwrap(), Duration::from_secs(43_200));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
        assert!(parse_duration("7").is_err());
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("1w").is_err());
    }

    #[test]
    fn human_durations_round_up() {
        assert_eq!(human(7 * 86_400 - 5), "7d");
        assert_eq!(human(86_400), "24h");
        assert_eq!(human(90), "2m");
        assert_eq!(human(1), "1m");
    }

    #[test]
    fn join_urls() {
        assert_eq!(
            join_url("https://x.test/", "abc"),
            "https://x.test/join/abc"
        );
        assert_eq!(join_url("https://x.test", "abc"), "https://x.test/join/abc");
    }

    #[tokio::test]
    async fn invite_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = Store::open(&dir.path().join("supp.db"))
            .await
            .unwrap()
            .accounts();

        let mut out = Vec::new();
        let ttl = Duration::from_secs(3600);
        invite_create(
            &accounts,
            "https://x.test",
            ttl,
            1,
            "partner".into(),
            &mut out,
        )
        .await
        .unwrap();
        let printed = String::from_utf8(out).unwrap();
        let link = printed.lines().nth(1).unwrap();
        let token = link.strip_prefix("https://x.test/join/").unwrap();

        // The printed link works.
        let identity = Identity {
            subject: "test:p".into(),
            email: "p@example.com".into(),
            name: "P".into(),
        };
        let login = accounts
            .login(&identity, Some(&Token::from(token.to_string())))
            .await
            .unwrap();

        let mut out = Vec::new();
        invite_list(&accounts, &mut out).await.unwrap();
        let listed = String::from_utf8(out).unwrap();
        assert!(listed.contains("account invite"));
        assert!(listed.contains("used up"));
        assert!(listed.contains("partner"));

        let mut out = Vec::new();
        user_list(&accounts, &mut out).await.unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("P <p@example.com>")
        );

        user_disable(&accounts, login.user.id.0, &mut Vec::new())
            .await
            .unwrap();
        assert!(
            user_disable(&accounts, login.user.id.0, &mut Vec::new())
                .await
                .is_err()
        );
        assert!(invite_revoke(&accounts, 99, &mut Vec::new()).await.is_err());
    }
}
