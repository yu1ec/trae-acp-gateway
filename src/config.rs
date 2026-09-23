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

/// Common install locations for CLI tools when the process inherits a
/// minimal PATH (e.g. Tauri / LaunchAgent on macOS omits `~/.local/bin`).
fn common_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = user_home_dir() {
        dirs.push(PathBuf::from(&home).join(".local").join("bin"));
        dirs.push(PathBuf::from(&home).join(".cargo").join("bin"));
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    #[cfg(windows)]
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Programs").join("trae").join("bin"));
    }
    dirs
}

/// Resolve a bare agent command name to an executable path.
pub fn resolve_trae_cmd(cmd: &str) -> String {
    if cmd.contains('/') || cmd.contains('\\') {
        return cmd.to_string();
    }

    #[cfg(windows)]
    let file_name = if cmd.ends_with(".exe") {
        cmd.to_string()
    } else {
        format!("{cmd}.exe")
    };
    #[cfg(not(windows))]
    let file_name = cmd.to_string();

    for dir in common_bin_dirs() {
        let path = dir.join(&file_name);
        if path.is_file() {
            return path.display().to_string();
        }
    }
    cmd.to_string()
}

/// PATH value to hand to spawned agent processes so `traecli` and its
/// helpers remain discoverable under a stripped parent environment.
pub fn agent_path_env() -> Option<String> {
    let extra = common_bin_dirs()
        .into_iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(if cfg!(windows) { ";" } else { ":" });
    match std::env::var("PATH") {
        Ok(path) if !path.is_empty() => Some(format!("{extra}{}{path}", if cfg!(windows) { ";" } else { ":" })),
        _ => Some(extra),
    }
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
#[command(
    name = "trae_acp_gateway",
    about = "OpenAI-compatible gateway over an ACP agent (trae CLI)",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(flatten)]
    pub config: Config,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, clap::Subcommand)]
pub enum Commands {
    /// Check for and apply updates from GitHub Releases
    Update {
        #[command(subcommand)]
        action: UpdateAction,
    },
}

#[derive(Debug, clap::Subcommand)]
pub enum UpdateAction {
    /// Check whether a newer release is available
    Check,
    /// Download and replace the current CLI binary
    Apply {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, clap::Args)]
pub struct Config {
    /// TCP port to listen on (localhost only).
    #[arg(long, default_value_t = 60111)]
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

    /// Check for updates in the background when starting the gateway.
    #[arg(long)]
    pub auto_check_update: bool,

    /// Disable automatic update checks (overrides settings file).
    #[arg(long, conflicts_with = "auto_check_update")]
    pub no_auto_check_update: bool,
}

impl Config {
    pub fn should_auto_check_update(&self) -> bool {
        if self.auto_check_update {
            return true;
        }
        if self.no_auto_check_update {
            return false;
        }
        crate::update::CliSettings::load().auto_check_update
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_exposes_update_subcommand() {
        let cmd = Cli::command();
        assert!(cmd.find_subcommand("update").is_some());
    }

    #[test]
    fn cli_parses_update_check() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["trae_acp_gateway", "update", "check"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Update { .. })));
    }

    #[test]
    fn resolve_trae_cmd_keeps_explicit_paths() {
        assert_eq!(resolve_trae_cmd("/opt/bin/traecli"), "/opt/bin/traecli");
        assert_eq!(resolve_trae_cmd(r"C:\Tools\traecli.exe"), r"C:\Tools\traecli.exe");
    }

    #[test]
    fn resolve_trae_cmd_finds_local_install() {
        let home = user_home_dir().expect("HOME");
        let candidate = PathBuf::from(&home).join(".local").join("bin").join("traecli");
        if candidate.is_file() {
            assert_eq!(resolve_trae_cmd("traecli"), candidate.display().to_string());
        }
    }
}
