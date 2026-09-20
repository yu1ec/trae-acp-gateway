//! GitHub Releases update checks and asset resolution.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};
use semver::Version;
use serde::Deserialize;

pub const GITHUB_OWNER: &str = "yu1ec";
pub const GITHUB_REPO: &str = "trae-acp-gateway";
pub const GITHUB_REPO_URL: &str = "https://github.com/yu1ec/trae-acp-gateway";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Show a desktop notification when a newer release is available (CLI / headless use).
pub fn notify_update_available(current: &str, latest: &str, release_url: &str) {
    let title = "Trae ACP Gateway 发现新版本";
    let body = format!("v{current} → v{latest}，运行 update apply 或前往设置页安装");

    #[cfg(target_os = "macos")]
    {
        let body_escaped = body.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            r#"display notification "{body_escaped}" with title "{title}""#
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args([title, &body])
            .spawn();
    }

    #[cfg(target_os = "windows")]
    {
        let _ = title;
        let _ = body;
    }

    eprintln!("{title}: v{current} -> v{latest}");
    eprintln!("Release: {release_url}");
    eprintln!("Run: trae_acp_gateway update apply");
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: String,
    pub release_url: String,
    pub assets: Vec<AssetInfo>,
}

#[derive(Debug, Clone)]
pub struct AssetInfo {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_url: String,
    pub asset_name: Option<String>,
    pub download_url: Option<String>,
}

pub fn is_update_available(current: &str, latest: &str) -> anyhow::Result<bool> {
    let current = parse_version(current)?;
    let latest = parse_version(latest)?;
    Ok(latest > current)
}

fn parse_version(raw: &str) -> anyhow::Result<Version> {
    let trimmed = raw.trim().trim_start_matches('v');
    Version::parse(trimmed).with_context(|| format!("invalid semver `{raw}`"))
}

pub fn cli_asset_name() -> anyhow::Result<&'static str> {
    if cfg!(target_os = "macos") {
        Ok("trae_acp_gateway-macos-universal")
    } else if cfg!(target_os = "linux") {
        Ok("trae_acp_gateway-linux-x86_64")
    } else if cfg!(target_os = "windows") {
        Ok("trae_acp_gateway-windows-x86_64.exe")
    } else {
        bail!("unsupported platform for CLI updates")
    }
}

pub fn app_installer_asset_name() -> anyhow::Result<&'static str> {
    if cfg!(target_os = "macos") {
        Ok("Trae-ACP-Gateway-macos-universal.dmg")
    } else if cfg!(target_os = "linux") {
        Ok("Trae-ACP-Gateway-linux-x86_64.AppImage")
    } else if cfg!(target_os = "windows") {
        Ok("Trae-ACP-Gateway-windows-x86_64.msi")
    } else {
        bail!("unsupported platform for App updates")
    }
}

pub async fn fetch_latest_release() -> anyhow::Result<ReleaseInfo> {
    let url = format!(
        "https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest"
    );
    let client = http_client();
    let mut req = client.get(&url).header("User-Agent", "trae-acp-gateway");
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
    }
    let release: GitHubRelease = req
        .send()
        .await
        .context("requesting GitHub latest release")?
        .error_for_status()
        .context("GitHub API returned an error")?
        .json()
        .await
        .context("decoding GitHub release JSON")?;

    let version = release.tag_name.trim_start_matches('v').to_string();
    Ok(ReleaseInfo {
        version,
        release_url: release.html_url,
        assets: release
            .assets
            .into_iter()
            .map(|a| AssetInfo {
                name: a.name,
                download_url: a.browser_download_url,
                size: a.size,
            })
            .collect(),
    })
}

pub fn find_asset<'a>(release: &'a ReleaseInfo, name: &str) -> Option<&'a AssetInfo> {
    release.assets.iter().find(|a| a.name == name)
}

pub async fn check_for_update(asset_name: &str) -> anyhow::Result<UpdateCheckResult> {
    let release = fetch_latest_release().await?;
    let current = CURRENT_VERSION.to_string();
    let update_available = is_update_available(&current, &release.version)?;
    let asset = find_asset(&release, asset_name);
    Ok(UpdateCheckResult {
        current_version: current,
        latest_version: Some(release.version.clone()),
        update_available,
        release_url: release.release_url.clone(),
        asset_name: asset.map(|a| a.name.clone()),
        download_url: asset.map(|a| a.download_url.clone()),
    })
}

pub async fn download_to_temp(url: &str, filename: &str) -> anyhow::Result<PathBuf> {
    let client = http_client();
    let resp = client
        .get(url)
        .header("User-Agent", "trae-acp-gateway")
        .send()
        .await
        .context("downloading release asset")?
        .error_for_status()
        .context("download request failed")?;

    let dir = std::env::temp_dir().join("trae-acp-gateway-updates");
    std::fs::create_dir_all(&dir).context("creating update temp dir")?;
    let path = dir.join(filename);

    let bytes = resp
        .bytes()
        .await
        .context("reading download response body")?;
    std::fs::write(&path, &bytes).with_context(|| format!("writing `{}`", path.display()))?;
    Ok(path)
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("trae-acp-gateway")
        .build()
        .expect("reqwest client")
}

/// Path to CLI settings: `~/.config/trae-acp-gateway/settings.json`.
pub fn cli_settings_path() -> anyhow::Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var("APPDATA").context("APPDATA not set")?
    } else {
        let home = crate::user_home_dir().context("HOME not set")?;
        PathBuf::from(home)
            .join(".config")
            .display()
            .to_string()
    };
    Ok(Path::new(&base).join("trae-acp-gateway").join("settings.json"))
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CliSettings {
    #[serde(default)]
    pub auto_check_update: bool,
}

impl CliSettings {
    pub fn load() -> Self {
        let path = match cli_settings_path() {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = cli_settings_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

pub fn current_exe() -> anyhow::Result<PathBuf> {
    std::env::current_exe().context("resolving current executable path")
}

pub async fn apply_cli_update(yes: bool) -> anyhow::Result<()> {
    let asset_name = cli_asset_name()?;
    let check = check_for_update(asset_name).await?;
    if !check.update_available {
        println!(
            "Already up to date (v{}).",
            check.current_version
        );
        return Ok(());
    }
    let Some(download_url) = check.download_url else {
        bail!("release v{} has no asset `{asset_name}`", check.latest_version.unwrap_or_default());
    };
    let latest = check
        .latest_version
        .clone()
        .unwrap_or_default();
    if !yes {
        eprintln!(
            "Update available: v{} -> v{latest}",
            check.current_version,
        );
        eprint!("Download and replace current binary? [y/N] ");
        use std::io::{self, Write as _};
        io::stderr().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            bail!("update cancelled");
        }
    }

    let path = download_to_temp(&download_url, asset_name).await?;
    replace_cli_binary(&path)?;
    println!("Updated to v{latest}. Restart the gateway to use the new binary.");
    Ok(())
}

fn replace_cli_binary(downloaded: &Path) -> anyhow::Result<()> {
    let exe = current_exe()?;
    #[cfg(windows)]
    {
        return replace_cli_binary_windows(&exe, downloaded);
    }
    #[cfg(not(windows))]
    {
        let backup = exe.with_extension("bak");
        if backup.exists() {
            std::fs::remove_file(&backup)?;
        }
        std::fs::rename(&exe, &backup).with_context(|| {
            format!("backing up `{}`", exe.display())
        })?;
        std::fs::copy(downloaded, &exe).with_context(|| {
            format!("installing update to `{}`", exe.display())
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = std::fs::metadata(&exe)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&exe, perms)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
fn replace_cli_binary_windows(exe: &Path, downloaded: &Path) -> anyhow::Result<()> {
    let script = std::env::temp_dir().join("trae-acp-gateway-update.bat");
    let content = format!(
        r#"@echo off
timeout /t 2 /nobreak >nul
copy /Y "{}" "{}"
start "" "{}"
del "%~f0"
"#,
        downloaded.display(),
        exe.display(),
        exe.display()
    );
    std::fs::write(&script, content)?;
    std::process::Command::new("cmd")
        .args(["/C", "start", "", script.to_str().unwrap_or("")])
        .spawn()
        .context("launching Windows update helper")?;
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_compare() {
        assert!(is_update_available("0.1.0", "0.2.0").unwrap());
        assert!(!is_update_available("0.2.0", "0.2.0").unwrap());
        assert!(!is_update_available("0.3.0", "0.2.0").unwrap());
    }

    #[test]
    fn strip_v_prefix() {
        assert!(is_update_available("v0.1.0", "v0.2.0").unwrap());
    }
}
