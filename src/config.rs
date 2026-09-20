//! Gateway configuration (CLI args only, per requirements).

use std::path::PathBuf;

use clap::Parser;

/// Subdirectory under the user home used as the default agent workdir.
pub const DEFAULT_WORKDIR_NAME: &str = "trae-acp-gateway";

/// User home directory (`HOME` on Unix, `USERPROFILE` on Windows).
pub fn user_home_dir() -> Option<String> {
    let key = if cfg!(windows) {
        "USERPROFILE"
    } else {
        "HOME"
    };
    std::env::var(key).ok()
}

/// Default agent workdir: `~/trae-acp-gateway` (or `%USERPROFILE%\\trae-acp-gateway`), or `.` if home is unknown.
pub fn default_workdir() -> String {
    user_home_dir()
        .map(|home| {
            PathBuf::from(home)
                .join(DEFAULT_WORKDIR_NAME)
                .display()
                .to_string()
        })
        .unwrap_or_else(|| ".".into())
}

/// Values that should be replaced with [`default_workdir`] (legacy / unset).
pub fn is_legacy_workdir(workdir: &str) -> bool {
    matches!(workdir.trim(), "" | "." | "./" | "~")
}

/// Create the workdir tree if missing (`~` is expanded first).
pub fn ensure_workdir(workdir: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(expand_workdir(workdir))
}

/// Expand `~` / `~/…` before canonicalization (GUI and CLI share this).
pub fn expand_workdir(workdir: &str) -> String {
    if workdir == "~" {
        return default_workdir();
    }
    if let Some(rest) = workdir.strip_prefix("~/") {
        if let Some(home) = user_home_dir() {
            let path = if rest.is_empty() {
                PathBuf::from(&home)
            } else {
                PathBuf::from(&home).join(rest)
            };
            return path.display().to_string();
        }
    }
    workdir.to_string()
}

#[derive(Debug, Parser)]
#[command(name = "trae_acp_gateway", about = "OpenAI-compatible gateway over an ACP agent (trae CLI)")]
pub struct Config {
    /// TCP port to listen on (localhost only).
    #[arg(long, default_value_t = 8080)]
    pub port: u16,

    /// Working directory handed to the agent (sessions and tools operate here).
    #[arg(long, default_value_t = default_workdir())]
    pub workdir: String,

    /// Agent executable to spawn.
    #[arg(long, default_value = "traecli")]
    pub trae_cmd: String,

    /// Arguments passed to the agent executable.
    #[arg(long, value_delimiter = ',', default_value = "acp,serve")]
    pub trae_args: Vec<String>,

    /// Keep the agent inside its sandbox: deny permission requests that
    /// escalate beyond it instead of approving them. Disable with `--sandbox=false`.
    #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
    pub sandbox: bool,

    /// Log every raw JSON-RPC line exchanged with the agent.
    #[arg(long)]
    pub debug: bool,
}
