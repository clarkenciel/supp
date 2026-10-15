use std::{io::stdout, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use clap::{Parser, Subcommand, ValueEnum};
use supp_server::{
    AppState, Docs, Store, admin, app,
    auth::{AuthConfig, DevProvider, GoogleProvider, IdentityProvider},
    docs::DEFAULT_COMPACT_AFTER,
};
use tracing_subscriber::EnvFilter;

#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
        /// Public base URL (used for OAuth redirects and invite links).
        #[arg(long, env = "SUPP_PUBLIC_URL", default_value = "http://127.0.0.1:3000")]
        public_url: String,
        /// Built web client to serve (crates/web/dist after `trunk build`).
        #[arg(long, env = "SUPP_STATIC_DIR")]
        static_dir: Option<PathBuf>,
        /// Identity provider.
        #[arg(long, env = "SUPP_AUTH", value_enum)]
        auth: AuthKind,
        /// OAuth client ID (required with --auth google).
        #[arg(long, env = "SUPP_GOOGLE_CLIENT_ID")]
        google_client_id: Option<String>,
        /// OAuth client secret (required with --auth google).
        #[arg(long, env = "SUPP_GOOGLE_CLIENT_SECRET", hide_env_values = true)]
        google_client_secret: Option<String>,
    },
    /// Manage invites and users, and back up the database.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum AuthKind {
    /// Sign in with Google (OpenID Connect).
    Google,
    /// Anyone signs in as any name. Local development only.
    Dev,
}

#[derive(Subcommand)]
enum AdminCommand {
    #[command(subcommand)]
    Invite(InviteCommand),
    #[command(subcommand)]
    User(UserCommand),
    /// Write a consistent copy of the database (safe while serving).
    Backup {
        /// Where to write the copy.
        path: PathBuf,
        /// Replace the file if it exists.
        #[arg(long)]
        force: bool,
    },
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
        Command::Serve {
            listen,
            public_url,
            static_dir,
            auth,
            google_client_id,
            google_client_secret,
        } => {
            let provider: Arc<dyn IdentityProvider> = match auth {
                AuthKind::Google => {
                    let (Some(id), Some(secret)) = (google_client_id, google_client_secret) else {
                        anyhow::bail!(
                            "--auth google needs SUPP_GOOGLE_CLIENT_ID and SUPP_GOOGLE_CLIENT_SECRET"
                        );
                    };
                    Arc::new(GoogleProvider::new(id, secret)?)
                }
                AuthKind::Dev => {
                    tracing::warn!("DEV AUTH ENABLED: anyone can sign in as anyone");
                    Arc::new(DevProvider)
                }
            };
            let docs = Arc::new(Docs::new(store.clone(), DEFAULT_COMPACT_AFTER));
            let mut state =
                AppState::new(&store, docs, AuthConfig::new(provider, &public_url)).await?;
            state.static_dir = static_dir;
            let listener = tokio::net::TcpListener::bind(listen).await?;
            tracing::info!("listening on {listen}, public URL {public_url}");
            axum::serve(listener, app(state))
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
                AdminCommand::Backup { path, force } => {
                    admin::backup(&store, &path, force, out).await?
                }
            }
        }
    }
    store.close().await;
    Ok(())
}
