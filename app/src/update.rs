//! App-side update helpers: download installer and launch it.

use std::path::Path;

use anyhow::Context as _;
use trae_acp_gateway::{
    app_installer_asset_name, check_for_update, download_to_temp, UpdateCheckResult,
    CURRENT_VERSION,
};

pub async fn check_app_update() -> anyhow::Result<UpdateCheckResult> {
    let asset = app_installer_asset_name()?;
    check_for_update(asset).await
}

pub async fn download_and_install() -> anyhow::Result<()> {
    let asset_name = app_installer_asset_name()?;
    let info = check_for_update(asset_name).await?;
    if !info.update_available {
        anyhow::bail!("already up to date (v{CURRENT_VERSION})");
    }
    let Some(url) = info.download_url else {
        anyhow::bail!("release has no installer asset `{asset_name}`");
    };
    let path = download_to_temp(&url, asset_name).await?;
    launch_installer(&path)?;
    Ok(())
}

fn launch_installer(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .context("opening DMG installer")?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("msiexec")
            .args(["/i", path.to_str().unwrap_or("")])
            .spawn()
            .context("launching MSI installer")?;
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .context("launching AppImage installer")?;
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = path;
        anyhow::bail!("unsupported platform for installer launch");
    }

    Ok(())
}
