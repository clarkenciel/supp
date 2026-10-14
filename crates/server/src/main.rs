use std::{io::stdout, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use clap::{Parser, Subcommand};
use supp_server::{AppState, Docs, Store, admin, app, docs::DEFAULT_COMPACT_AFTER};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// SQLite database file.
    #[arg(long, global = true, env = "SUPP_DB", default_value = "supp.db")]
    db: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server.
    Serve {
        #[arg(long, env = "SUPP_LISTEN", default_value = "127.0.0.1:3000")]
        listen: SocketAddr,
    },
    /// Manage invites and users.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    #[command(subcommand)]
    Invite(InviteCommand),
    #[command(subcommand)]
    User(UserCommand),
}

#[derive(Subcommand)]
enum InviteCommand {
    /// Create an account invite and print its link (shown once).
    Create {
        /// How long the link stays valid, e.g. 30m, 12h, 7d.
        #[arg(long, default_value = "7d", value_parser = admin::parse_duration)]
        expires: Duration,
        /// How many accounts the link can create.
        #[arg(long, default_value_t = 1)]
        uses: u32,
        /// Reminder of who the invite is for.
        #[arg(long, default_value = "")]
        note: String,
        /// Public base URL of the server, used to build the link.
        #[arg(long, env = "SUPP_PUBLIC_URL")]
        public_url: String,
    },
    /// List account invites and share links.
    List,
    /// Revoke an invite.
    Revoke { id: i64 },
}

#[derive(Subcommand)]
enum UserCommand {
    /// List users and who invited them.
    List,
    /// Disable a user: ends their sessions and revokes their invites.
    Disable { id: i64 },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    let store = Store::open(&cli.db).await?;
    match cli.command {
        Command::Serve { listen } => {
            let docs = Arc::new(Docs::new(store.clone(), DEFAULT_COMPACT_AFTER));
            let listener = tokio::net::TcpListener::bind(listen).await?;
            tracing::info!("listening on {listen}");
            axum::serve(listener, app(AppState { docs }))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
        Command::Admin { command } => {
            let accounts = store.accounts();
            let out = &mut stdout();
            match command {
                AdminCommand::Invite(InviteCommand::Create {
                    expires,
                    uses,
                    note,
                    public_url,
                }) => {
                    admin::invite_create(&accounts, &public_url, expires, uses, note, out).await?
                }
                AdminCommand::Invite(InviteCommand::List) => {
                    admin::invite_list(&accounts, out).await?
                }
                AdminCommand::Invite(InviteCommand::Revoke { id }) => {
                    admin::invite_revoke(&accounts, id, out).await?
                }
                AdminCommand::User(UserCommand::List) => admin::user_list(&accounts, out).await?,
                AdminCommand::User(UserCommand::Disable { id }) => {
                    admin::user_disable(&accounts, id, out).await?
                }
            }
        }
    }
    store.close().await;
    Ok(())
}
