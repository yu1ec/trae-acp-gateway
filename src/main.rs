use clap::Parser as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cfg = trae_acp_gateway::Config::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                if cfg.debug {
                    "trae_acp_gateway=debug".into()
                } else {
                    "trae_acp_gateway=info".into()
                }
            }),
        )
        .init();

    trae_acp_gateway::canonicalize_workdir(&mut cfg)?;
    let handle = trae_acp_gateway::serve(std::sync::Arc::new(cfg)).await?;
    handle.await?;
    Ok(())
}
