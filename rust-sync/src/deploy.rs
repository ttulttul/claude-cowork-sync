use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use log::info;

pub fn atomic_swap_profile(
    live_profile: &Path,
    merged_profile: &Path,
    backup_parent: &Path,
) -> Result<PathBuf> {
    if !live_profile.exists() {
        bail!("Live profile not found: {}", live_profile.display());
    }
    if !merged_profile.exists() {
        bail!("Merged profile not found: {}", merged_profile.display());
    }

    fs::create_dir_all(backup_parent)
        .with_context(|| format!("Failed to create backup parent {}", backup_parent.display()))?;

    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let backup_path = backup_parent.join(format!(
        "{}.backup.{}",
        live_profile
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Claude"),
        timestamp
    ));

    info!(
        "Moving live profile {} to backup {}",
        live_profile.display(),
        backup_path.display()
    );
    fs::rename(live_profile, &backup_path).with_context(|| {
        format!(
            "Failed to move live profile {} to {}",
            live_profile.display(),
            backup_path.display()
        )
    })?;

    info!(
        "Promoting merged profile {} to live path {}",
        merged_profile.display(),
        live_profile.display()
    );
    fs::rename(merged_profile, live_profile).with_context(|| {
        format!(
            "Failed to promote merged profile {} to {}",
            merged_profile.display(),
            live_profile.display()
        )
    })?;

    Ok(backup_path)
}
