//! App-side update helpers.
//!
//! The Tauri updater plugin owns the manifest/version/signature flow; this
//! module only keeps the legacy GitHub API check as a fallback for releases
//! that predate updater artifacts, and exposes the mirror-aware downloader.

use std::path::PathBuf;

use anyhow::Context as _;
use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};
use trae_acp_gateway::{
    UpdateCheckResult, app_installer_asset_name, check_for_update, download_with_mirrors,
};

/// GitHub API fallback used when the release has no `latest.json`.
pub async fn check_app_update() -> anyhow::Result<UpdateCheckResult> {
    let asset = app_installer_asset_name()?;
    check_for_update(asset).await
}

/// Download an updater artifact (`.app.tar.gz`, `.nsis.zip`, ...) to a temp
/// file, optionally trying the configured GitHub mirrors first.
pub async fn download_update_artifact(
    url: &str,
    filename: &str,
    accelerate: bool,
    on_progress: impl FnMut(u64, Option<u64>, u64),
) -> anyhow::Result<PathBuf> {
    download_with_mirrors(url, filename, accelerate, on_progress)
        .await
        .context("downloading update artifact")
}

/// Verify the minisign signature from `latest.json` before installing.
///
/// The plugin only verifies inside its own `download()`, which we bypass to
/// support GitHub mirrors, so `install()` would otherwise accept any bytes.
/// Mirrors are therefore not trusted: a tampered artifact fails here.
pub fn verify_update_signature(
    data: &[u8],
    release_signature: &str,
    pub_key: &str,
    announced_version: &str,
) -> anyhow::Result<()> {
    let decode = |value: &str| -> anyhow::Result<String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .context("base64 decoding signature material")?;
        String::from_utf8(bytes).context("signature material is not UTF-8")
    };

    let public_key = PublicKey::decode(&decode(pub_key)?).context("decoding updater public key")?;
    let signature =
        Signature::decode(&decode(release_signature)?).context("decoding update signature")?;

    public_key
        .verify(data, &signature, true)
        .context("update signature verification failed")?;

    // The manifest itself is not signed. Requiring the version embedded in the
    // signed trusted comment stops a tampered manifest from pairing an inflated
    // version with an older artifact's URL and signature (a downgrade replay).
    let signed = signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("version:"))
        .context("update signature does not carry a signed version")?;

    let normalize = |v: &str| v.trim().trim_start_matches('v').to_string();
    if normalize(signed) != normalize(announced_version) {
        anyhow::bail!(
            "signed version {signed} does not match announced version {announced_version}"
        );
    }

    Ok(())
}
