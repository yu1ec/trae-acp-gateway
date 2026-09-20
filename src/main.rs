use clap::Parser as _;

use trae_acp_gateway::config::{Cli, Commands, UpdateAction};
use trae_acp_gateway::update::{self, cli_asset_name};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                if cli.config.debug {
                    "trae_acp_gateway=debug".into()
                } else {
                    "trae_acp_gateway=info".into()
                }
            }),
        )
        .init();

    match cli.command {
        Some(Commands::Update { action }) => run_update(action).await,
        None => run_serve(cli.config).await,
    }
}

async fn run_update(action: UpdateAction) -> anyhow::Result<()> {
    match action {
        UpdateAction::Check => {
            let asset = cli_asset_name()?;
            let info = update::check_for_update(asset).await?;
            println!("Current version: v{}", info.current_version);
            if let Some(latest) = &info.latest_version {
                println!("Latest version:  v{latest}");
            }
            println!("Release: {}", info.release_url);
            if info.update_available {
                if let Some(name) = &info.asset_name {
                    println!("Download asset: {name}");
                }
                std::process::exit(1);
            } else {
                println!("Already up to date.");
            }
        }
        UpdateAction::Apply { yes } => {
            update::apply_cli_update(yes).await?;
        }
    }
    Ok(())
}

async fn run_serve(mut cfg: trae_acp_gateway::Config) -> anyhow::Result<()> {
    if cfg.should_auto_check_update() {
        tokio::spawn(async {
            match cli_asset_name() {
                Ok(asset) => {
                    if let Ok(info) = update::check_for_update(asset).await {
                        if info.update_available {
                            if let Some(latest) = info.latest_version {
                                tracing::info!(
                                    current = %info.current_version,
                                    latest = %latest,
                                    url = %info.release_url,
                                    "update available"
                                );
                                update::notify_update_available(
                                    &info.current_version,
                                    &latest,
                                    &info.release_url,
                                );
                            }
                        }
                    }
                }
                Err(e) => tracing::debug!("auto update check skipped: {e}"),
            }
        });
    }

    trae_acp_gateway::canonicalize_workdir(&mut cfg)?;
    let handle = trae_acp_gateway::serve(std::sync::Arc::new(cfg)).await?;
    handle.await?;
    Ok(())
}
