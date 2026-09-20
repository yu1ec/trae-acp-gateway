pub mod acp;
pub mod agent;
pub mod config;
pub mod http;
pub mod openai;

use std::sync::Arc;

use anyhow::Context as _;
use axum::routing::{get, post};
use axum::Router;
pub use config::{
    default_workdir, ensure_workdir, expand_workdir, is_legacy_workdir, user_home_dir,
    Config, DEFAULT_WORKDIR_NAME,
};

pub fn router(cfg: Arc<Config>) -> Router {
    Router::new()
        .route("/v1/models", get(http::models))
        // OpenAI clients pick streaming via `stream: true`; we route on it.
        .route("/v1/chat/completions", post(route_chat))
        .with_state(cfg)
}

/// Bind and serve the gateway on a background task; abort the returned handle to stop.
/// In-flight streaming requests are dropped on abort (accepted for v1).
pub async fn serve(cfg: Arc<Config>) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    tracing::info!(%addr, cmd = %cfg.trae_cmd, args = ?cfg.trae_args, "starting gateway");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let app = router(cfg);
    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "gateway server error");
        }
    }))
}

/// ACP agents reject relative cwd (`session/new`: -32602), so normalize once.
pub fn canonicalize_workdir(cfg: &mut Config) -> anyhow::Result<()> {
    let expanded = expand_workdir(&cfg.workdir);
    std::fs::create_dir_all(&expanded)
        .with_context(|| format!("creating workdir `{expanded}`"))?;
    cfg.workdir = std::fs::canonicalize(&expanded)
        .with_context(|| format!("resolving workdir `{expanded}`"))?
        .display()
        .to_string();
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
