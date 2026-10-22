//! Gateway configuration (CLI args only, per requirements).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use clap::Parser;

/// User home directory (`HOME` on Unix, `USERPROFILE` on Windows).
pub fn user_home_dir() -> Option<String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(key).ok()
}

/// Values that mean "no workdir configured".
pub fn is_legacy_workdir(workdir: &str) -> bool {
    matches!(workdir.trim(), "" | "." | "./")
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
        // Current TRAE CLI 2.0 installer locations.
        dirs.push(
            PathBuf::from(&local)
                .join("Programs")
                .join("TraeCLI")
                .join("bin"),
        );
        dirs.push(
            PathBuf::from(&local)
                .join("Programs")
                .join("TraeX")
                .join("bin"),
        );
        dirs.push(
            PathBuf::from(local)
                .join("Programs")
                .join("trae")
                .join("bin"),
        );
    }
    dirs
}

/// Windows installs `.cmd` shims (`traecli.cmd` -> `traex.exe`). `Command`
/// cannot spawn batch files directly, so return the real executable.
#[cfg(windows)]
pub(crate) fn spawnable_path(path: PathBuf) -> PathBuf {
    let is_shim = matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("cmd" | "bat" | "ps1")
    );
    if is_shim {
        let exe = path.with_extension("exe");
        if exe.is_file() {
            return exe;
        }
        let traex = path.with_file_name("traex.exe");
        if traex.is_file() {
            return traex;
        }
    }
    path
}

fn find_in_dir(dir: &std::path::Path, cmd: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    let names = [
        cmd.to_string(),
        format!("{cmd}.exe"),
        format!("{cmd}.cmd"),
        format!("{cmd}.bat"),
    ];
    #[cfg(not(windows))]
    let names = [cmd.to_string()];

    for name in names {
        let path = dir.join(name);
        if path.is_file() {
            #[cfg(windows)]
            return Some(spawnable_path(path));
            #[cfg(not(windows))]
            return Some(path);
        }
    }
    None
}

/// Resolve a bare agent command name to an executable path.
pub fn resolve_trae_cmd(cmd: &str) -> String {
    if cmd.contains('/') || cmd.contains('\\') {
        return cmd.to_string();
    }
    for dir in common_bin_dirs() {
        if let Some(path) = find_in_dir(&dir, cmd) {
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
        Ok(path) if !path.is_empty() => Some(format!(
            "{extra}{}{path}",
            if cfg!(windows) { ";" } else { ":" }
        )),
        _ => Some(extra),
    }
}

/// Expand `~` / `~/…` before canonicalization (GUI and CLI share this).
pub fn expand_workdir(workdir: &str) -> String {
    if workdir == "~" {
        return user_home_dir().unwrap_or_else(|| ".".into());
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

/// Request header clients may use to report their own working directory.
pub const WORKDIR_HEADER: &str = "x-trae-workdir";

/// Default allowlist root for client-reported workdirs: the user's home.
pub fn default_workdir_root() -> String {
    user_home_dir().unwrap_or_else(|| {
        if cfg!(windows) {
            "C:\\".into()
        } else {
            "/".into()
        }
    })
}

/// Where a per-request workdir came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkdirSource {
    Header,
    Body,
    EnvironmentContext,
    SystemPrompt,
    Config,
}

impl WorkdirSource {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkdirSource::Header => "header",
            WorkdirSource::Body => "body",
            WorkdirSource::EnvironmentContext => "environment",
            WorkdirSource::SystemPrompt => "system_prompt",
            WorkdirSource::Config => "config",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkdir {
    pub path: String,
    pub source: WorkdirSource,
}

/// Resolve the workdir for one request.
///
/// Priority: explicit header > request body field > Claude Code environment
/// context > client system prompt > configured default. Explicit sources are
/// rejected when invalid; hint sources are best-effort and fall back to the
/// configured workdir.
pub fn resolve_request_workdir(
    header: Option<&str>,
    body: Option<&str>,
    environment_context: Option<&str>,
    system_prompt: Option<&str>,
    configured: &str,
    allowed_root: &str,
) -> anyhow::Result<ResolvedWorkdir> {
    for (raw, source) in [(header, WorkdirSource::Header), (body, WorkdirSource::Body)] {
        if let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) {
            return Ok(ResolvedWorkdir {
                path: validate_workdir(raw, allowed_root)?,
                source,
            });
        }
    }

    let hints = [
        (
            environment_context.and_then(workdir_from_environment_context),
            WorkdirSource::EnvironmentContext,
        ),
        (
            system_prompt.and_then(workdir_from_system_prompt),
            WorkdirSource::SystemPrompt,
        ),
    ];
    for (raw, source) in hints {
        let Some(raw) = raw else { continue };
        match validate_workdir(&raw, allowed_root) {
            Ok(path) => return Ok(ResolvedWorkdir { path, source }),
            Err(e) => {
                tracing::warn!(workdir = %raw, error = %e, "ignoring workdir hint");
            }
        }
    }

    Ok(ResolvedWorkdir {
        path: configured.to_string(),
        source: WorkdirSource::Config,
    })
}

/// Pull the cwd out of the environment block Claude Code injects into user
/// turns (`# Environment` / `- Primary working directory: ...`).
///
/// The block is client-supplied, so callers must still enforce the allowlist.
/// Requiring both the harness marker and the exact field keeps ordinary user
/// text from being mistaken for environment metadata.
pub fn workdir_from_environment_context(text: &str) -> Option<String> {
    const MARKER: &str = "You have been invoked in the following environment";
    const PREFIX: &str = "Primary working directory:";
    if !text.contains("# Environment") || !text.contains(MARKER) {
        return None;
    }
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("- ").unwrap_or(line).trim();
        let Some(rest) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let value = rest
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\'' | '`'))
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Pull `Working directory: <path>` out of a client system prompt.
pub fn workdir_from_system_prompt(system: &str) -> Option<String> {
    const PREFIX: &str = "Working directory:";
    for line in system.lines() {
        let line = line.trim();
        let line = line.strip_prefix("- ").unwrap_or(line).trim();
        let Some(rest) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let value = rest
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\'' | '`'))
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Validate a client-supplied workdir: absolute, existing directory, and
/// inside `allowed_root` after resolving symlinks.
fn validate_workdir(raw: &str, allowed_root: &str) -> anyhow::Result<String> {
    let expanded = expand_workdir(raw);
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        bail!("workdir `{raw}` must be an absolute path");
    }
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("workdir `{expanded}` does not exist or is inaccessible"))?;
    if !canonical.is_dir() {
        bail!("workdir `{}` is not a directory", canonical.display());
    }

    let root = if allowed_root.trim().is_empty() {
        PathBuf::from(default_workdir_root())
    } else {
        PathBuf::from(expand_workdir(allowed_root.trim()))
    };
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    if !canonical.starts_with(&root) {
        bail!(
            "workdir `{}` is outside allowed root `{}`",
            canonical.display(),
            root.display()
        );
    }
    Ok(canonical.display().to_string())
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

    /// Working directory handed to the agent when the client does not report
    /// one. Defaults to the gateway process's current directory.
    #[arg(long, default_value = "")]
    pub workdir: String,

    /// Only client-reported workdirs under this directory are accepted.
    #[arg(long, default_value_t = default_workdir_root())]
    pub allowed_workdir_root: String,

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
        assert_eq!(
            resolve_trae_cmd(r"C:\Tools\traecli.exe"),
            r"C:\Tools\traecli.exe"
        );
    }

    #[test]
    fn resolve_trae_cmd_finds_local_install() {
        let home = user_home_dir().expect("HOME");
        let candidate = PathBuf::from(&home)
            .join(".local")
            .join("bin")
            .join("traecli");
        if candidate.is_file() {
            assert_eq!(resolve_trae_cmd("traecli"), candidate.display().to_string());
        }
    }
}

#[cfg(test)]
mod workdir_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// Fresh directory under the system temp dir, already canonicalized.
    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "trae-acp-gateway-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    #[test]
    fn parses_working_directory_line() {
        let system = "<env>\nWorking directory: /tmp/project\nIs directory a git repo: Yes\n</env>";
        assert_eq!(
            workdir_from_system_prompt(system).as_deref(),
            Some("/tmp/project")
        );
    }

    #[test]
    fn parses_quoted_and_dashed_working_directory() {
        assert_eq!(
            workdir_from_system_prompt("- Working directory: \"/tmp/a b\"\n").as_deref(),
            Some("/tmp/a b")
        );
    }

    #[test]
    fn ignores_system_prompt_without_workdir() {
        assert_eq!(workdir_from_system_prompt("no cwd here"), None);
    }

    fn environment_block(path: &str) -> String {
        format!(
            "# Environment\nYou have been invoked in the following environment: \n - Primary working directory: {path}\n - Is a git repository: false\n"
        )
    }

    #[test]
    fn parses_environment_context() {
        assert_eq!(
            workdir_from_environment_context(&environment_block("/tmp/project")).as_deref(),
            Some("/tmp/project")
        );
        assert_eq!(
            workdir_from_environment_context("Primary working directory: /x"),
            None
        );
        assert_eq!(
            workdir_from_environment_context("# Environment\nnothing here"),
            None
        );
    }

    #[test]
    fn header_wins_over_body_and_hints() {
        let dir = temp_dir();
        let dir_str = dir.display().to_string();
        let resolved = resolve_request_workdir(
            Some(&dir_str),
            Some("relative/body"),
            Some(&environment_block("/definitely/not/here")),
            Some("Working directory: /definitely/not/here"),
            "/configured",
            &dir_str,
        )
        .unwrap();
        assert_eq!(resolved.source, WorkdirSource::Header);
        assert_eq!(resolved.path, dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn body_wins_over_hints() {
        let dir = temp_dir();
        let dir_str = dir.display().to_string();
        let resolved = resolve_request_workdir(
            None,
            Some(&dir_str),
            Some(&environment_block("/definitely/not/here")),
            Some("Working directory: /definitely/not/here"),
            "/configured",
            &dir_str,
        )
        .unwrap();
        assert_eq!(resolved.source, WorkdirSource::Body);
        assert_eq!(resolved.path, dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn environment_context_wins_over_system_prompt() {
        let dir = temp_dir();
        let dir_str = dir.display().to_string();
        let resolved = resolve_request_workdir(
            None,
            None,
            Some(&environment_block(&dir_str)),
            Some("Working directory: /definitely/not/here"),
            "/configured",
            &dir_str,
        )
        .unwrap();
        assert_eq!(resolved.source, WorkdirSource::EnvironmentContext);
        assert_eq!(resolved.path, dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn system_prompt_hint_is_used_when_valid() {
        let dir = temp_dir();
        let dir_str = dir.display().to_string();
        let system = format!("Working directory: {dir_str}");
        let resolved =
            resolve_request_workdir(None, None, None, Some(&system), "/configured", &dir_str)
                .unwrap();
        assert_eq!(resolved.source, WorkdirSource::SystemPrompt);
        assert_eq!(resolved.path, dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hint_outside_root_falls_back_to_config() {
        let dir = temp_dir();
        let dir_str = dir.display().to_string();
        let outside = if cfg!(windows) { "C:\\" } else { "/" };
        let resolved = resolve_request_workdir(
            None,
            None,
            Some(&environment_block(outside)),
            Some(&format!("Working directory: {outside}")),
            "/configured",
            &dir_str,
        )
        .unwrap();
        assert_eq!(resolved.source, WorkdirSource::Config);
        assert_eq!(resolved.path, "/configured");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explicit_relative_workdir_is_rejected() {
        let err =
            resolve_request_workdir(Some("relative/dir"), None, None, None, "/configured", "/")
                .unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[test]
    fn explicit_workdir_outside_root_is_rejected() {
        if cfg!(windows) {
            return; // C:\ is the parent of every test temp dir on Windows.
        }
        let dir = temp_dir();
        let dir_str = dir.display().to_string();
        let err = resolve_request_workdir(Some("/"), None, None, None, "/configured", &dir_str)
            .unwrap_err();
        assert!(err.to_string().contains("outside allowed root"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
