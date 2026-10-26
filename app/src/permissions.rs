//! macOS privacy/TCC helpers for the settings panel.
//!
//! The gateway itself only needs to read the configured workdir and write its
//! own config. Full Disk Access is optional: it is only useful when the agent
//! has to work inside protected locations (Desktop, Documents, Downloads,
//! removable volumes, or other TCC-protected data). These helpers let the UI
//! explain that and jump straight to the relevant System Settings pane.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use serde::Serialize;
use trae_acp_gateway::user_home_dir;

#[derive(Debug, Clone, Serialize)]
pub struct PermissionStatus {
    /// `macos`, `linux`, or `windows`.
    pub platform: String,
    /// `None` when it cannot be determined (or on non-macOS platforms).
    pub full_disk_access: Option<bool>,
    /// Absolute path of the `.app` bundle (or install directory off macOS).
    pub app_path: String,
    /// True when macOS launched the app from an App Translocation path. TCC
    /// grants and in-place updates do not work from there.
    pub translocated: bool,
    /// True when the app lives in `/Applications` or `~/Applications`.
    pub in_applications: bool,
}

pub fn status() -> PermissionStatus {
    let exe = std::env::current_exe().ok();
    let app_path = app_bundle_path(exe.as_deref());
    let translocated = app_path.contains("/AppTranslocation/");
    let in_applications = is_in_applications(&app_path);

    PermissionStatus {
        platform: std::env::consts::OS.to_string(),
        full_disk_access: full_disk_access(),
        app_path,
        translocated,
        in_applications,
    }
}

fn app_bundle_path(exe: Option<&Path>) -> String {
    let Some(exe) = exe else {
        return String::new();
    };

    #[cfg(target_os = "macos")]
    {
        for ancestor in exe.ancestors() {
            if ancestor.extension().and_then(|ext| ext.to_str()) == Some("app") {
                return ancestor.display().to_string();
            }
        }
    }

    exe.parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default()
}

fn is_in_applications(app_path: &str) -> bool {
    if app_path.starts_with("/Applications/") {
        return true;
    }
    user_home_dir().is_some_and(|home| {
        let user_apps = PathBuf::from(home).join("Applications");
        Path::new(app_path).starts_with(user_apps)
    })
}

/// macOS only lets a process read the TCC database after Full Disk Access has
/// been granted. Opening it is a side-effect-free probe: without the grant the
/// kernel returns `EPERM`, and no permission dialog is shown.
///
/// Deliberately probes only the TCC database. Reading other protected paths
/// (Safari bookmarks, Mail) can itself trigger a consent prompt, which would
/// make merely opening the settings page invasive.
#[cfg(target_os = "macos")]
fn full_disk_access() -> Option<bool> {
    let home = user_home_dir()?;
    let path = PathBuf::from(&home).join("Library/Application Support/com.apple.TCC/TCC.db");
    if !path.exists() {
        return None;
    }
    match std::fs::File::open(&path) {
        Ok(_) => Some(true),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Some(false),
        Err(_) => None,
    }
}

#[cfg(not(target_os = "macos"))]
fn full_disk_access() -> Option<bool> {
    None
}

/// Open the System Settings pane for one privacy category.
pub fn open_settings(pane: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let url = match pane {
            "full-disk-access" => {
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles"
            }
            "files-and-folders" => {
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders"
            }
            "automation" => {
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Automation"
            }
            "notifications" => {
                "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
            }
            other => bail!("unknown permission pane `{other}`"),
        };
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .context("opening System Settings")?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        bail!("System permission settings are only available on macOS");
    }
}

/// Reveal the `.app` bundle in Finder so the user can drag it into the Full
/// Disk Access list (System Settings has no API to add it programmatically).
pub fn reveal_in_finder() -> anyhow::Result<()> {
    let path = app_bundle_path(std::env::current_exe().ok().as_deref());
    if path.is_empty() {
        bail!("cannot determine the application path");
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path])
            .spawn()
            .context("revealing the application in Finder")?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        bail!("Finder reveal is only available on macOS");
    }
}

/// Clear a stray Media & Apple Music grant. The gateway never needs the media
/// library; macOS creates that entry when an agent command walks a protected
/// directory. Resetting it removes the entry until it is triggered again.
pub fn reset_media_library() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("tccutil")
            .args(["reset", "MediaLibrary", "com.trae.acp-gateway"])
            .status()
            .context("running tccutil")?;
        if !status.success() {
            bail!("tccutil exited with {status}");
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        bail!("tccutil is only available on macOS");
    }
}
