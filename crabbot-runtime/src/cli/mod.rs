// src/cli/mod.rs
use clap::{Parser, Subcommand};
use crabbot_shared::DEFAULT_SESSION_KEY;
use tokio_util::sync::CancellationToken;

use crate::{error::Result, runtime};

mod tui;

#[derive(Parser, Debug)]
#[command(name = "crabbot")]
pub struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Runtime {
        #[command(subcommand)]
        cmd: RuntimeCmd,
    },
    Chat,
}

#[derive(Subcommand, Debug)]
enum RuntimeCmd {
    Run,
}

pub async fn entrypoint() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Runtime {
            cmd: RuntimeCmd::Run,
        } => {
            tracing::info!("Starting crabbot runtime...");
            let cancel = CancellationToken::new();

            let engine = match runtime::create().await {
                Ok(e) => {
                    tracing::info!("Runtime engine created successfully");
                    e
                }
                Err(e) => {
                    tracing::error!("Failed to create runtime engine: {e}");
                    return Err(e);
                }
            };

            // Register signal handlers for graceful shutdown.
            // In a container under tini, SIGTERM is the normal stop signal.
            let cancel_for_signal = cancel.clone();
            tokio::spawn(async move {
                let mut sigterm =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("failed to register SIGTERM handler");

                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        tracing::info!("Received SIGINT, shutting down runtime...");
                    }
                    _ = sigterm.recv() => {
                        tracing::info!("Received SIGTERM, shutting down runtime...");
                    }
                }
                cancel_for_signal.cancel();
            });

            tracing::info!("Runtime engine running — waiting for work...");
            let result = engine.run(cancel.clone()).await;

            match &result {
                Ok(()) => tracing::info!("Runtime engine stopped gracefully"),
                Err(e) => tracing::error!("Runtime engine stopped with error: {e}"),
            }

            result
        }

        Cmd::Chat => {
            let cancel = CancellationToken::new();
            let engine = runtime::create().await?;

            // run engine in background
            let engine_task = {
                let engine = engine.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move { engine.run(cancel).await })
            };

            // run TUI on blocking thread
            let tui_task = tokio::task::spawn_blocking({
                let engine = engine.clone();
                let cancel = cancel.clone();
                move || tui::run_blocking(engine, DEFAULT_SESSION_KEY.to_string(), cancel)
            });

            tokio::select! {
                _ = tokio::signal::ctrl_c() => { cancel.cancel(); }
                r = engine_task => { let _ = r; }
                r = tui_task => { let _ = r; cancel.cancel(); }
            }

            Ok(())
        }
    }
}
