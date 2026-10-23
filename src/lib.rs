pub mod acp;
pub mod agent;
pub mod anthropic;
pub mod config;
pub mod gemini;
pub mod http;
pub mod openai;
pub mod openai_responses;
pub mod prompt;
pub mod thought;
pub mod trae_cli;
pub mod update;

use std::sync::Arc;

use anyhow::Context as _;
use axum::Router;
use axum::routing::{get, post};
pub use config::{
    Cli, Commands, Config, ResolvedWorkdir, UpdateAction, WORKDIR_HEADER, WorkdirSource,
    default_workdir_root, ensure_workdir, expand_workdir, is_legacy_workdir,
    resolve_request_workdir, user_home_dir, workdir_from_environment_context,
    workdir_from_system_prompt,
};
pub use openai_responses::{GatewayState, ResponseStore};
pub use thought::{ThoughtSink, ThoughtUpdate};
pub use update::{
    CURRENT_VERSION, CliSettings, GITHUB_OWNER, GITHUB_REPO, GITHUB_REPO_URL, UpdateCheckResult,
    app_installer_asset_name, apply_cli_update, candidate_download_urls, check_for_update,
    cli_asset_name, download_to_temp, download_with_mirrors, notify_update_available,
};

/// Host-side hook reporting the number of ACP turns currently in flight. It is
/// called with the new count whenever a turn starts or finishes, so a host can
/// render a "communicating" indicator.
pub type ActivitySink = Arc<dyn Fn(usize) + Send + Sync>;

pub type AppState = Arc<GatewayState>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/models", get(http::models))
        .route("/v1/chat/completions", post(route_chat))
        .route("/v1/messages", post(anthropic::messages))
        .route("/v1/responses", post(openai_responses::create_response))
        .route("/v1/responses/{id}", get(openai_responses::get_response))
        // matchit forbids `{model}:generateContent` (colon is a second param).
        .route(
            "/v1beta/models/{spec}",
            post(gemini::route_generate_content),
        )
        .route("/v1beta/models", get(gemini::list_models))
        .with_state(state)
}

/// Bind `127.0.0.1:port`. Fails immediately when the port is taken; there is
/// no automatic fallback, so the configured port is always the one served.
pub async fn bind_localhost(port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}; port may already be in use"))
}

/// Bind and serve the gateway on a background task; abort the returned handle to stop.
/// Returns the task handle and the port actually bound (`cfg.port`, or the
/// OS-assigned port when `cfg.port` is 0).
pub async fn serve(cfg: Arc<Config>) -> anyhow::Result<(tokio::task::JoinHandle<()>, u16)> {
    serve_with_thoughts(cfg, None, None).await
}

/// Like [`serve`], but forwards ACP reasoning chunks to `thought_sink` and
/// in-flight turn counts to `activity_sink`. The CLI and any headless user
/// keeps using [`serve`].
pub async fn serve_with_thoughts(
    cfg: Arc<Config>,
    thought_sink: Option<ThoughtSink>,
    activity_sink: Option<ActivitySink>,
) -> anyhow::Result<(tokio::task::JoinHandle<()>, u16)> {
    let listener = bind_localhost(cfg.port)
        .await
        .with_context(|| format!("starting gateway on 127.0.0.1:{}", cfg.port))?;
    let addr = listener.local_addr()?;
    let port = addr.port();
    tracing::info!(%addr, cmd = %cfg.trae_cmd, args = ?cfg.trae_args, "starting gateway");
    let state = Arc::new(GatewayState {
        config: cfg.clone(),
        responses: ResponseStore::default(),
        thought_sink,
        activity: Default::default(),
        activity_sink,
    });
    let app = router(state);
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "gateway server error");
        }
    });
    Ok((handle, port))
}

/// ACP agents reject relative cwd (`session/new`: -32602), so normalize once.
/// An unset workdir falls back to the gateway process's current directory.
pub fn canonicalize_workdir(cfg: &mut Config) -> anyhow::Result<()> {
    let configured = cfg.workdir.trim();
    let expanded = if configured.is_empty() {
        std::env::current_dir()
            .context("resolving current directory")?
            .display()
            .to_string()
    } else {
        expand_workdir(configured)
    };
    std::fs::create_dir_all(&expanded).with_context(|| format!("creating workdir `{expanded}`"))?;
    cfg.workdir = std::fs::canonicalize(&expanded)
        .with_context(|| format!("resolving workdir `{expanded}`"))?
        .display()
        .to_string();
    if cfg.allowed_workdir_root.trim().is_empty() {
        cfg.allowed_workdir_root = config::default_workdir_root();
    }
    let root = expand_workdir(cfg.allowed_workdir_root.trim());
    if let Ok(root) = std::fs::canonicalize(&root) {
        cfg.allowed_workdir_root = root.display().to_string();
    }
    let resolved = config::resolve_trae_cmd(&cfg.trae_cmd);
    if resolved != cfg.trae_cmd {
        tracing::info!(from = %cfg.trae_cmd, to = %resolved, "resolved agent command");
    }
    cfg.trae_cmd = resolved;
    Ok(())
}

async fn route_chat(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    req: axum::Json<openai::ChatRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if req.stream {
        http::chat_completions(axum::extract::State(state), headers, req)
            .await
            .into_response()
    } else {
        http::chat_completions_buffered(axum::extract::State(state), headers, req)
            .await
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::bind_localhost;

    #[tokio::test]
    async fn bind_localhost_fails_fast_when_port_is_taken() {
        let listener = bind_localhost(0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let err = bind_localhost(port).await.unwrap_err();
        assert!(format!("{err:#}").contains("already in use"), "{err:?}");
        drop(listener);
    }
}
