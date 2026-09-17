mod acp;
mod agent;
mod config;
mod http;
mod openai;

use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser as _;
use axum::routing::{get, post};
use axum::Router;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cfg = config::Config::parse();

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

    // ACP agents reject relative cwd (`session/new`: -32602), so normalize once.
    cfg.workdir = std::fs::canonicalize(&cfg.workdir)
        .with_context(|| format!("resolving --workdir `{}`", cfg.workdir))?
        .display()
        .to_string();
    let cfg = Arc::new(cfg);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    tracing::info!(%addr, cmd = %cfg.trae_cmd, args = ?cfg.trae_args, "starting gateway");

    let app = Router::new()
        .route("/v1/models", get(http::models))
        // OpenAI clients pick streaming via `stream: true`; we route on it.
        .route("/v1/chat/completions", post(route_chat))
        .with_state(cfg);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn route_chat(
    axum::extract::State(cfg): axum::extract::State<http::SharedConfig>,
    req: axum::Json<openai::ChatRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if req.stream {
        http::chat_completions(axum::extract::State(cfg), req)
            .await
            .into_response()
    } else {
        http::chat_completions_buffered(axum::extract::State(cfg), req)
            .await
            .into_response()
    }
}
