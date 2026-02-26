use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use log::{error, info, warn};

use crate::browser_storage::{merge_browser_states, read_browser_state, write_browser_state};
use crate::fs_merge::merge_session_trees;
use crate::models::{SessionBinding, SessionMergeResult, ValidationResult};
use crate::validate::validate_merged_profile;

#[derive(Debug, Clone)]
pub struct MergeSummary {
    pub output_profile: PathBuf,
    pub merged_session_count: usize,
    pub browser_state_output: Option<PathBuf>,
    pub validation: ValidationResult,
}

#[derive(Debug, Clone)]
pub struct MergeOptions {
    pub profile_a: PathBuf,
    pub profile_b: PathBuf,
    pub output_profile: PathBuf,
    pub include_sensitive_claude_credentials: bool,
    pub base_source: String,
    pub browser_state_a_path: Option<PathBuf>,
    pub browser_state_b_path: Option<PathBuf>,
    pub browser_state_output_path: Option<PathBuf>,
    pub merge_indexeddb: bool,
    pub skip_browser_state: bool,
    pub force_output_overwrite: bool,
    pub include_vm_bundles: bool,
    pub include_cache_dirs: bool,
}

pub fn merge_profiles(options: &MergeOptions) -> Result<MergeSummary> {
    if options.include_vm_bundles {
        info!(
            "include_vm_bundles affects remote fetch only; local vm_bundles are always preserved"
        );
    }

    validate_input_profiles(&options.profile_a, &options.profile_b)?;
    prepare_output_profile(
        &options.profile_a,
        &options.output_profile,
        options.force_output_overwrite,
        options.include_cache_dirs,
    )?;

    let merged_sessions = merge_session_trees(
        &options.profile_a,
        &options.profile_b,
        &options.output_profile,
        options.include_sensitive_claude_credentials,
    )?;

    let (browser_output, merged_local_storage) = if options.skip_browser_state {
        (None, HashMap::new())
    } else {
        merge_browser_state_files(options, &merged_sessions)?
    };

    let validation = validate_merged_profile(
        &options.output_profile,
        &merged_sessions,
        &merged_local_storage,
        !options.skip_browser_state,
    );

    Ok(MergeSummary {
        output_profile: options.output_profile.clone(),
        merged_session_count: merged_sessions.len(),
        browser_state_output: browser_output,
        validation,
    })
}

fn validate_input_profiles(profile_a: &Path, profile_b: &Path) -> Result<()> {
    for profile in [profile_a, profile_b] {
        if !profile.exists() {
            error!("Profile path does not exist: {}", profile.display());
            bail!("Profile path does not exist: {}", profile.display());
        }
        if !profile.is_dir() {
            error!("Profile path is not a directory: {}", profile.display());
            bail!("Profile path is not a directory: {}", profile.display());
        }
    }
    Ok(())
}

fn prepare_output_profile(
    profile_a: &Path,
    output_profile: &Path,
    force_output_overwrite: bool,
    include_cache_dirs: bool,
) -> Result<()> {
    if output_profile.exists() {
        if !force_output_overwrite {
            error!(
                "Output profile already exists: {}",
                output_profile.display()
            );
            bail!(
                "Output profile already exists: {}",
                output_profile.display()
            );
        }
        warn!(
            "Removing existing output profile: {}",
            output_profile.display()
        );
        fs::remove_dir_all(output_profile)
            .with_context(|| format!("Failed to remove {}", output_profile.display()))?;
    }

    info!(
        "Copying base profile to output: {}",
        output_profile.display()
    );
    if !include_cache_dirs {
        info!("Excluding non-essential cache directories from base profile copy");
    }

    copy_profile_tree(profile_a, output_profile, include_cache_dirs)?;
    Ok(())
}

fn copy_profile_tree(
    profile_a: &Path,
    output_profile: &Path,
    include_cache_dirs: bool,
) -> Result<()> {
    let excluded_rel_paths = build_profile_copy_excludes(include_cache_dirs);
    copy_tree_filtered(profile_a, output_profile, &excluded_rel_paths)
}

fn build_profile_copy_excludes(include_cache_dirs: bool) -> HashSet<String> {
    let mut excluded_rel_paths: HashSet<String> = HashSet::new();
    if !include_cache_dirs {
        for item in [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnCache",
            "GrShaderCache",
            "ShaderCache",
            "Service Worker/CacheStorage",
            "Service Worker/ScriptCache",
            "Network/Cache",
        ] {
            excluded_rel_paths.insert(item.to_string());
        }
    }
    excluded_rel_paths
}

fn copy_tree_filtered(
    src_root: &Path,
    dst_root: &Path,
    excluded_rel_paths: &HashSet<String>,
) -> Result<()> {
    fs::create_dir_all(dst_root)
        .with_context(|| format!("Failed to create destination root {}", dst_root.display()))?;
    copy_tree_filtered_inner(src_root, src_root, dst_root, excluded_rel_paths)
}

fn copy_tree_filtered_inner(
    src_root: &Path,
    current_src: &Path,
    dst_root: &Path,
    excluded_rel_paths: &HashSet<String>,
) -> Result<()> {
    for entry in fs::read_dir(current_src)
        .with_context(|| format!("Failed to read directory {}", current_src.display()))?
    {
        let entry =
            entry.with_context(|| format!("Failed to read entry in {}", current_src.display()))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(src_root)
            .with_context(|| format!("Failed to compute relative path for {}", path.display()))?;
        let relative_posix = relative.to_string_lossy().replace('\\', "/");

        if excluded_rel_paths.contains(&relative_posix) {
            continue;
        }

        let destination = dst_root.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to stat {}", path.display()))?;

        if metadata.file_type().is_dir() {
            fs::create_dir_all(&destination)
                .with_context(|| format!("Failed to create directory {}", destination.display()))?;
            copy_tree_filtered_inner(src_root, &path, dst_root, excluded_rel_paths)?;
            continue;
        }

        if metadata.file_type().is_symlink() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let target = fs::read_link(&path)
                    .with_context(|| format!("Failed to read symlink {}", path.display()))?;
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create {}", parent.display()))?;
                }
                symlink(&target, &destination).with_context(|| {
                    format!(
                        "Failed to copy symlink {} -> {}",
                        path.display(),
                        destination.display()
                    )
                })?;
            }
            continue;
        }

        if metadata.file_type().is_file() {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create parent {}", parent.display()))?;
            }
            fs::copy(&path, &destination).with_context(|| {
                format!(
                    "Failed to copy file {} to {}",
                    path.display(),
                    destination.display()
                )
            })?;
            fs::set_permissions(&destination, metadata.permissions()).with_context(|| {
                format!(
                    "Failed to preserve permissions on {}",
                    destination.display()
                )
            })?;
        }
    }

    Ok(())
}

fn merge_browser_state_files(
    options: &MergeOptions,
    merged_sessions: &BTreeMap<String, SessionMergeResult>,
) -> Result<(Option<PathBuf>, HashMap<String, String>)> {
    let Some(browser_state_a_path) = options.browser_state_a_path.as_ref() else {
        bail!(
            "Browser state merge requires --browser-state-a, --browser-state-b, and --browser-state-output unless --skip-browser-state is set"
        );
    };
    let Some(browser_state_b_path) = options.browser_state_b_path.as_ref() else {
        bail!(
            "Browser state merge requires --browser-state-a, --browser-state-b, and --browser-state-output unless --skip-browser-state is set"
        );
    };
    let Some(browser_state_output_path) = options.browser_state_output_path.as_ref() else {
        bail!(
            "Browser state merge requires --browser-state-a, --browser-state-b, and --browser-state-output unless --skip-browser-state is set"
        );
    };

    let state_a = read_browser_state(browser_state_a_path)?;
    let state_b = read_browser_state(browser_state_b_path)?;

    let mut binding_map: HashMap<String, SessionBinding> = HashMap::new();
    for (session_id, result) in merged_sessions {
        binding_map.insert(session_id.clone(), result.binding.clone());
    }

    let merged = merge_browser_states(
        &state_a,
        &state_b,
        &binding_map,
        &options.base_source,
        profile_mtime_ms(&options.profile_a)?,
        profile_mtime_ms(&options.profile_b)?,
        options.merge_indexeddb,
    );
    write_browser_state(browser_state_output_path, &merged)?;
    Ok((
        Some(browser_state_output_path.clone()),
        merged.local_storage.clone(),
    ))
}

fn profile_mtime_ms(path: &Path) -> Result<i64> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("Failed to stat profile path {}", path.display()))?;
    let modified = metadata
        .modified()
        .with_context(|| format!("Failed to read modified time for {}", path.display()))?;
    let epoch = modified
        .duration_since(std::time::UNIX_EPOCH)
        .with_context(|| format!("Modified time before epoch for {}", path.display()))?;
    Ok(epoch.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::merge_engine::{merge_profiles, MergeOptions};

    #[test]
    fn merge_profiles_requires_browser_state_unless_skipped() -> Result<()> {
        let tmp = tempdir()?;
        let profile_a = create_minimal_profile(&tmp.path().join("a"))?;
        let profile_b = create_minimal_profile(&tmp.path().join("b"))?;

        let options = MergeOptions {
            profile_a,
            profile_b,
            output_profile: tmp.path().join("out"),
            include_sensitive_claude_credentials: false,
            base_source: "a".to_string(),
            browser_state_a_path: None,
            browser_state_b_path: None,
            browser_state_output_path: None,
            merge_indexeddb: false,
            skip_browser_state: false,
            force_output_overwrite: false,
            include_vm_bundles: false,
            include_cache_dirs: false,
        };

        let result = merge_profiles(&options);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn merge_profiles_succeeds_with_skip_browser_state() -> Result<()> {
        let tmp = tempdir()?;
        let profile_a = create_minimal_profile(&tmp.path().join("a"))?;
        let profile_b = create_minimal_profile(&tmp.path().join("b"))?;

        let options = MergeOptions {
            profile_a,
            profile_b,
            output_profile: tmp.path().join("out"),
            include_sensitive_claude_credentials: false,
            base_source: "a".to_string(),
            browser_state_a_path: None,
            browser_state_b_path: None,
            browser_state_output_path: None,
            merge_indexeddb: false,
            skip_browser_state: true,
            force_output_overwrite: false,
            include_vm_bundles: false,
            include_cache_dirs: false,
        };

        let summary = merge_profiles(&options)?;
        assert_eq!(summary.merged_session_count, 1);
        assert!(summary.validation.is_valid());
        Ok(())
    }

    #[test]
    fn merge_profiles_excludes_cache_dirs_by_default() -> Result<()> {
        let tmp = tempdir()?;
        let profile_a = create_minimal_profile(&tmp.path().join("a"))?;
        let profile_b = create_minimal_profile(&tmp.path().join("b"))?;
        let cache_file = profile_a.join("Code Cache/js/index.bin");
        if let Some(parent) = cache_file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&cache_file, b"cache")?;

        let options = MergeOptions {
            profile_a,
            profile_b,
            output_profile: tmp.path().join("out"),
            include_sensitive_claude_credentials: false,
            base_source: "a".to_string(),
            browser_state_a_path: None,
            browser_state_b_path: None,
            browser_state_output_path: None,
            merge_indexeddb: false,
            skip_browser_state: true,
            force_output_overwrite: false,
            include_vm_bundles: false,
            include_cache_dirs: false,
        };

        let summary = merge_profiles(&options)?;
        assert!(!summary.output_profile.join("Code Cache").exists());
        Ok(())
    }

    fn create_minimal_profile(profile: &Path) -> Result<PathBuf> {
        let session_root = profile.join("local-agent-mode-sessions/user/org");
        fs::create_dir_all(&session_root)?;
        let metadata = json!({
            "createdAt": 1,
            "lastActivityAt": 2,
            "cliSessionId": "cli",
            "cwd": "/tmp"
        });
        fs::write(
            session_root.join("local_x.json"),
            serde_json::to_string(&metadata)?,
        )?;
        let folder = session_root.join("local_x");
        fs::create_dir_all(&folder)?;
        fs::write(folder.join("audit.jsonl"), "")?;
        Ok(profile.to_path_buf())
    }

    use std::path::{Path, PathBuf};
}
