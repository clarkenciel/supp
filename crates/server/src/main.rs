use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use clap::{Parser, Subcommand};
use supp_server::{AppState, Docs, Store, app, docs::DEFAULT_COMPACT_AFTER};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server.
    Serve {
        /// SQLite database file.
        #[arg(long, env = "SUPP_DB", default_value = "supp.db")]
        db: PathBuf,
        #[arg(long, env = "SUPP_LISTEN", default_value = "127.0.0.1:3000")]
        listen: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    match Cli::parse().command {
        Command::Serve { db, listen } => {
            let store = Store::open(&db).await?;
            let docs = Arc::new(Docs::new(store.clone(), DEFAULT_COMPACT_AFTER));
            let listener = tokio::net::TcpListener::bind(listen).await?;
            tracing::info!("listening on {listen}");
            axum::serve(listener, app(AppState { docs }))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            store.close().await;
        }
    }
    Ok(())
}
