pub mod acp;
pub mod agent;
pub mod anthropic;
pub mod config;
pub mod gemini;
pub mod http;
pub mod openai;
pub mod openai_responses;
pub mod prompt;
pub mod update;

use std::sync::Arc;

use anyhow::Context as _;
use axum::routing::{get, post};
use axum::Router;
pub use config::{
    default_workdir, ensure_workdir, expand_workdir, is_legacy_workdir, user_home_dir, Cli,
    Commands, Config, UpdateAction, DEFAULT_WORKDIR_NAME,
};
pub use openai_responses::{GatewayState, ResponseStore};
pub use update::{
    app_installer_asset_name, check_for_update, cli_asset_name, download_to_temp,
    apply_cli_update, notify_update_available, CliSettings, UpdateCheckResult, CURRENT_VERSION,
    GITHUB_OWNER, GITHUB_REPO, GITHUB_REPO_URL,
};

pub type AppState = Arc<GatewayState>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/models", get(http::models))
        .route("/v1/chat/completions", post(route_chat))
        .route("/v1/messages", post(anthropic::messages))
        .route("/v1/responses", post(openai_responses::create_response))
        .route("/v1/responses/{id}", get(openai_responses::get_response))
        .route(
            "/v1beta/models/{model}:generateContent",
            post(gemini::generate_content),
        )
        .route(
            "/v1beta/models/{model}:streamGenerateContent",
            post(gemini::stream_generate_content),
        )
        .route("/v1beta/models", get(gemini::list_models))
        .with_state(state)
}

/// Bind and serve the gateway on a background task; abort the returned handle to stop.
pub async fn serve(cfg: Arc<Config>) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    tracing::info!(%addr, cmd = %cfg.trae_cmd, args = ?cfg.trae_args, "starting gateway");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let state = Arc::new(GatewayState {
        config: cfg.clone(),
        responses: ResponseStore::default(),
    });
    let app = router(state);
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
    let resolved = config::resolve_trae_cmd(&cfg.trae_cmd);
    if resolved != cfg.trae_cmd {
        tracing::info!(from = %cfg.trae_cmd, to = %resolved, "resolved agent command");
    }
    cfg.trae_cmd = resolved;
    Ok(())
}

async fn route_chat(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::Json<openai::ChatRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if req.stream {
        http::chat_completions(axum::extract::State(state), req)
            .await
            .into_response()
    } else {
        http::chat_completions_buffered(axum::extract::State(state), req)
            .await
            .into_response()
    }
}
