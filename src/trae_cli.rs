//! TRAE CLI (`traecli`) preflight: detect whether the agent is installed and,
//! when it is missing, offer to run the official installer.
//!
//! The gateway itself is only a protocol adapter; every request spawns
//! `traecli acp serve`. Starting without it means every HTTP turn fails, so
//! both the CLI and the desktop app check before bringing the server up.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, bail};

/// Official installer entry points (see the TRAE CLI quick start docs).
pub const DOCS_URL: &str = "https://docs.trae.cn/cli_about-trae-code-cli-2";
pub const INSTALL_SH_URL: &str = "https://trae.cn/trae-cli/install_v2.sh";
pub const INSTALL_PS1_URL: &str = "https://trae.cn/trae-cli/install_v2.ps1";

/// True for the default bare command name (`traecli` / `traecli.exe`).
/// Custom paths are never auto-installed: the installer would put the binary
/// somewhere the configured path does not point at.
pub fn is_default_traecli(cmd: &str) -> bool {
    if cmd.contains('/') || cmd.contains('\\') {
        return false;
    }
    let base = cmd
        .strip_suffix(".exe")
        .or_else(|| cmd.strip_suffix(".cmd"))
        .or_else(|| cmd.strip_suffix(".bat"))
        .unwrap_or(cmd);
    base.eq_ignore_ascii_case("traecli")
}

#[cfg(windows)]
fn candidate_names(cmd: &str) -> Vec<String> {
    if Path::new(cmd).extension().is_none() {
        return vec![
            cmd.to_string(),
            format!("{cmd}.exe"),
            format!("{cmd}.cmd"),
            format!("{cmd}.bat"),
        ];
    }
    vec![cmd.to_string()]
}

#[cfg(not(windows))]
fn candidate_names(cmd: &str) -> Vec<String> {
    vec![cmd.to_string()]
}

/// Locate the configured agent executable.
///
/// Bare names are looked up in the common install dirs first (GUI apps on
/// macOS inherit a stripped `PATH`), then in `PATH`. Explicit paths are only
/// checked for existence.
pub fn find_installed(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') || cmd.contains('\\') {
        let path = PathBuf::from(cmd);
        if !path.is_file() {
            return None;
        }
        #[cfg(windows)]
        return Some(crate::config::spawnable_path(path));
        #[cfg(not(windows))]
        return Some(path);
    }

    let resolved = crate::config::resolve_trae_cmd(cmd);
    if resolved != cmd {
        let path = PathBuf::from(&resolved);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(path_env) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_env) {
            for name in candidate_names(cmd) {
                let path = dir.join(name);
                if path.is_file() {
                    #[cfg(windows)]
                    return Some(crate::config::spawnable_path(path));
                    #[cfg(not(windows))]
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Human-readable install commands, matching the official docs.
pub fn install_commands() -> (&'static str, &'static str) {
    (
        "sh -c \"$(curl -fsSL https://trae.cn/trae-cli/install_v2.sh)\"",
        "irm https://trae.cn/trae-cli/install_v2.ps1 | iex",
    )
}

pub fn install_hint() -> String {
    let (unix, windows) = install_commands();
    format!("macOS/Linux: {unix}\nWindows:     {windows}\nDocs:        {DOCS_URL}")
}

/// Run the official installer for the current platform.
///
/// `TRAECLI_INSTALL_ASSUME_YES=1` keeps the installer non-interactive (it may
/// otherwise ask about PATH updates and dependency installs). Stdio is
/// inherited so the CLI user sees download progress.
pub fn install_trae_cli() -> anyhow::Result<()> {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("powershell");
        command.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!("irm {INSTALL_PS1_URL} | iex"),
        ]);
        command
    } else {
        let mut command = Command::new("sh");
        command.args(["-c", &format!("curl -fsSL {INSTALL_SH_URL} | sh")]);
        command
    };
    command.env("TRAECLI_INSTALL_ASSUME_YES", "1");

    // GUI builds on Windows have no console; avoid flashing one for the child.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let status = command.status().context("running the TRAE CLI installer")?;
    if !status.success() {
        bail!(
            "TRAE CLI installer exited with {status}; install manually:\n{}",
            install_hint()
        );
    }
    Ok(())
}

fn confirm(prompt: &str) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    print!("{prompt}");
    std::io::stdout().flush().context("writing prompt")?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading answer")?;
    let answer = line.trim();
    Ok(answer.is_empty() || matches!(answer, "y" | "Y" | "yes" | "YES" | "Yes"))
}

/// Ensure the agent exists, prompting to install it when running interactively.
/// Returns the resolved executable path.
pub fn ensure_installed(cmd: &str, interactive: bool) -> anyhow::Result<PathBuf> {
    if let Some(path) = find_installed(cmd) {
        return Ok(path);
    }

    if !is_default_traecli(cmd) {
        bail!(
            "agent executable `{cmd}` not found; fix `--trae-cmd` or install TRAE CLI:\n{}",
            install_hint()
        );
    }

    eprintln!("未检测到 TRAE CLI（`{cmd}`），Gateway 无法处理请求。");
    eprintln!("{}", install_hint());
    if !interactive {
        bail!("TRAE CLI 未安装；请在终端中重新运行以自动安装，或手动执行上面的命令");
    }
    if !confirm("是否现在自动安装 TRAE CLI？[Y/n] ")? {
        bail!("TRAE CLI 未安装，已取消启动");
    }

    install_trae_cli()?;
    find_installed(cmd).with_context(|| {
        format!("安装脚本已执行，但仍未找到 `{cmd}`；请确认安装目录已加入 PATH 后重试")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_command_detection() {
        assert!(is_default_traecli("traecli"));
        assert!(is_default_traecli("TRAECLI"));
        assert!(is_default_traecli("traecli.exe"));
        assert!(is_default_traecli("traecli.cmd"));
        assert!(!is_default_traecli("/opt/bin/traecli"));
        assert!(!is_default_traecli("./traecli"));
        assert!(!is_default_traecli("traex"));
    }

    #[test]
    fn explicit_missing_path_is_not_installed() {
        assert!(find_installed("/definitely/not/here/traecli").is_none());
    }

    #[test]
    fn explicit_existing_path_is_detected() {
        let path = std::env::temp_dir().join(format!("traecli-detect-test-{}", std::process::id()));
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        let found = find_installed(&path.display().to_string());
        let _ = std::fs::remove_file(&path);
        assert_eq!(found.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn hint_mentions_both_installers_and_docs() {
        let hint = install_hint();
        assert!(hint.contains(INSTALL_SH_URL));
        assert!(hint.contains(INSTALL_PS1_URL));
        assert!(hint.contains(DOCS_URL));
    }
}
