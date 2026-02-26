use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{bail, Context, Result};
use log::{debug, error, info, warn};
use rayon::prelude::*;
use regex::Regex;
use which::which;

use crate::progress::{format_bytes, run_with_spinner_result, ProgressColor, TerminalProgress};
use crate::utils::{sha1_file, sha256_file, sha256_text};

#[derive(Debug, Default)]
struct BaseSyncPlan {
    transfer_paths: Vec<String>,
    seed_from_baseline: Vec<String>,
    seed_from_cache: Vec<String>,
}

#[derive(Debug, Default)]
struct SessionSyncPlan {
    transfer_entries: Vec<(String, String)>,
    seed_from_cache_entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Sha256,
    Sha1,
}

impl HashAlgorithm {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sha256" => Ok(Self::Sha256),
            "sha1" => Ok(Self::Sha1),
            _ => bail!("Unsupported hash algorithm: {raw}"),
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha1 => "sha1",
        }
    }

    fn shasum_bits(self) -> &'static str {
        match self {
            Self::Sha256 => "256",
            Self::Sha1 => "1",
        }
    }
}

const NON_ESSENTIAL_CACHE_DIRS: [&str; 9] = [
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "GrShaderCache",
    "ShaderCache",
    "Service Worker/CacheStorage",
    "Service Worker/ScriptCache",
    "Network/Cache",
];

const NON_ESSENTIAL_CACHE_PATHS: [&str; 18] = [
    "$BASE_NAME/Cache",
    "$BASE_NAME/Cache/*",
    "$BASE_NAME/Code Cache",
    "$BASE_NAME/Code Cache/*",
    "$BASE_NAME/GPUCache",
    "$BASE_NAME/GPUCache/*",
    "$BASE_NAME/DawnCache",
    "$BASE_NAME/DawnCache/*",
    "$BASE_NAME/GrShaderCache",
    "$BASE_NAME/GrShaderCache/*",
    "$BASE_NAME/ShaderCache",
    "$BASE_NAME/ShaderCache/*",
    "$BASE_NAME/Service Worker/CacheStorage",
    "$BASE_NAME/Service Worker/CacheStorage/*",
    "$BASE_NAME/Service Worker/ScriptCache",
    "$BASE_NAME/Service Worker/ScriptCache/*",
    "$BASE_NAME/Network/Cache",
    "$BASE_NAME/Network/Cache/*",
];
const REMOTE_HASH_XARGS_BATCH_SIZE: usize = 32;

pub fn fetch_remote_profile(
    remote_host: &str,
    remote_profile_path: &str,
    temp_parent: Option<&Path>,
    include_vm_bundles: bool,
    baseline_profile: Option<&Path>,
    include_cache_dirs: bool,
    hash_algorithm: HashAlgorithm,
    parallel_remote: Option<usize>,
    parallel_local: usize,
) -> Result<PathBuf> {
    if remote_host.trim().is_empty() {
        bail!("Remote host must be a non-empty string");
    }

    ensure_ssh_available()?;
    ensure_remote_claude_not_running(remote_host)?;

    let target_root = create_target_root(temp_parent)?;
    info!(
        "Fetching remote profile from {}:{}",
        remote_host, remote_profile_path
    );
    if !include_cache_dirs {
        info!("Pruning non-essential cache directories from remote transfer");
    }

    if let Some(baseline) = baseline_profile {
        if baseline.exists() {
            fetch_remote_profile_incremental(
                remote_host,
                remote_profile_path,
                include_vm_bundles,
                &target_root,
                baseline,
                include_cache_dirs,
                hash_algorithm,
                parallel_remote,
                parallel_local,
            )?;
        } else {
            warn!(
                "Baseline profile does not exist ({}); falling back to full remote fetch",
                baseline.display()
            );
            fetch_remote_tar_with_command(
                remote_host,
                &build_remote_tar_command(
                    remote_profile_path,
                    include_vm_bundles,
                    include_cache_dirs,
                    false,
                )?,
                &target_root,
                "Remote fetch (full profile)",
            )?;
        }
    } else {
        fetch_remote_tar_with_command(
            remote_host,
            &build_remote_tar_command(
                remote_profile_path,
                include_vm_bundles,
                include_cache_dirs,
                false,
            )?,
            &target_root,
            "Remote fetch (full profile)",
        )?;
    }

    let fetched_path = target_root.join(remote_profile_name(remote_profile_path)?);
    if !fetched_path.exists() {
        bail!(
            "Fetched profile not found after transfer: {}",
            fetched_path.display()
        );
    }

    Ok(fetched_path)
}

fn fetch_remote_profile_incremental(
    remote_host: &str,
    remote_profile_path: &str,
    include_vm_bundles: bool,
    target_root: &Path,
    baseline_profile: &Path,
    include_cache_dirs: bool,
    hash_algorithm: HashAlgorithm,
    parallel_remote: Option<usize>,
    parallel_local: usize,
) -> Result<()> {
    info!(
        "Using incremental remote fetch against baseline: {}",
        baseline_profile.display()
    );

    let remote_name = remote_profile_name(remote_profile_path)?;
    let incremental_target_root = target_root.join(&remote_name);
    fs::create_dir_all(&incremental_target_root).with_context(|| {
        format!(
            "Failed to create incremental target root {}",
            incremental_target_root.display()
        )
    })?;
    let base_cache_root = resolve_remote_base_cache_path(remote_host, remote_profile_path)?;
    fs::create_dir_all(&base_cache_root).with_context(|| {
        format!(
            "Failed to create remote base-file cache directory {}",
            base_cache_root.display()
        )
    })?;

    let remote_base_hashes = list_remote_non_session_file_hashes(
        remote_host,
        remote_profile_path,
        include_vm_bundles,
        include_cache_dirs,
        hash_algorithm,
        parallel_remote,
    )?;

    let mut base_diff_progress = TerminalProgress::new(
        "Base diff",
        if remote_base_hashes.is_empty() {
            None
        } else {
            Some(remote_base_hashes.len() as u64)
        },
        "files",
        ProgressColor::Yellow,
    );
    let base_plan = plan_remote_base_sync(
        &remote_base_hashes,
        baseline_profile,
        &base_cache_root,
        hash_algorithm,
        parallel_local,
        Some(&mut base_diff_progress),
    );
    base_diff_progress.finish(
        remote_base_hashes.len() as u64,
        &format!(
            "transfer_paths={} seed_local={} seed_cache={}",
            base_plan.transfer_paths.len(),
            base_plan.seed_from_baseline.len(),
            base_plan.seed_from_cache.len()
        ),
        true,
    );
    info!(
        "Incremental base diff: remote_files={} transfer_paths={} seed_local={} seed_cache={}",
        remote_base_hashes.len(),
        base_plan.transfer_paths.len(),
        base_plan.seed_from_baseline.len(),
        base_plan.seed_from_cache.len()
    );

    seed_files_from_source(
        baseline_profile,
        &incremental_target_root,
        &base_plan.seed_from_baseline,
        "Base seed (local)",
    )?;
    seed_files_from_source(
        &base_cache_root,
        &incremental_target_root,
        &base_plan.seed_from_cache,
        "Base seed (cache)",
    )?;

    if !base_plan.transfer_paths.is_empty() {
        fetch_remote_tar_with_path_list(
            remote_host,
            &build_remote_tar_from_path_list_command(remote_profile_path)?,
            &incremental_target_root,
            &base_plan.transfer_paths,
            "Remote fetch (base profile)",
        )?;
    }
    sync_remote_base_cache(
        &base_cache_root,
        baseline_profile,
        &incremental_target_root,
        &base_plan,
    )?;

    let remote_hashes = list_remote_session_json_hashes(
        remote_host,
        remote_profile_path,
        hash_algorithm,
        parallel_remote,
    )?;
    let session_cache_root = resolve_remote_session_cache_path(remote_host, remote_profile_path)?;
    fs::create_dir_all(&session_cache_root).with_context(|| {
        format!(
            "Failed to create remote session cache directory {}",
            session_cache_root.display()
        )
    })?;

    let mut session_diff_progress = TerminalProgress::new(
        "Session diff",
        if remote_hashes.is_empty() {
            None
        } else {
            Some(remote_hashes.len() as u64)
        },
        "sessions",
        ProgressColor::Magenta,
    );
    let session_plan = plan_remote_session_sync(
        &remote_hashes,
        baseline_profile,
        &session_cache_root,
        hash_algorithm,
        parallel_local,
        Some(&mut session_diff_progress),
    );
    session_diff_progress.finish(
        remote_hashes.len() as u64,
        &format!(
            "transfer_sessions={} seed_cache={}",
            session_plan.transfer_entries.len(),
            session_plan.seed_from_cache_entries.len()
        ),
        true,
    );
    info!(
        "Incremental session diff: remote_sessions={} transfer_sessions={} seed_cache={}",
        remote_hashes.len(),
        session_plan.transfer_entries.len(),
        session_plan.seed_from_cache_entries.len()
    );

    seed_session_entries_from_cache(
        &session_cache_root,
        &incremental_target_root,
        &session_plan.seed_from_cache_entries,
    )?;

    if !session_plan.transfer_entries.is_empty() {
        fetch_remote_tar_with_path_list(
            remote_host,
            &build_remote_tar_from_path_list_command(remote_profile_path)?,
            &incremental_target_root,
            &session_entries_to_path_list(&session_plan.transfer_entries),
            "Remote fetch (session delta)",
        )?;
    }
    sync_remote_session_cache(
        &session_cache_root,
        &incremental_target_root,
        &session_plan.transfer_entries,
    )
}

fn ensure_ssh_available() -> Result<()> {
    which("ssh").with_context(|| "ssh command is required for --merge-from but was not found")?;
    Ok(())
}

fn ensure_remote_claude_not_running(remote_host: &str) -> Result<()> {
    let running = find_remote_processes_with_signature(remote_host, "Claude")?;
    if running.is_empty() {
        return Ok(());
    }

    bail!(
        "Found running Claude process(es) on remote host {}. Quit Claude on the remote machine and retry. Matches: {}",
        remote_host,
        running.join(", ")
    )
}

fn find_remote_processes_with_signature(remote_host: &str, signature: &str) -> Result<Vec<String>> {
    let completed = run_remote_ssh_output_with_spinner(
        remote_host,
        "ps -axo pid=,comm=,args=",
        "Remote preflight",
        &format!("checking remote process list on {remote_host}"),
        "checked",
        ProgressColor::Blue,
    )?;

    if !completed.status.success() {
        bail!(
            "Failed to list remote processes on {}: {}",
            remote_host,
            String::from_utf8_lossy(&completed.stderr).trim()
        );
    }

    let helper_pattern = Regex::new(r"Contents/Helpers/.+").expect("valid helper pattern");
    let stdout = String::from_utf8_lossy(&completed.stdout);
    let mut matches = Vec::new();

    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line
            .splitn(3, char::is_whitespace)
            .filter(|part| !part.is_empty());
        let Some(pid_str) = parts.next() else {
            continue;
        };
        if pid_str.parse::<i32>().is_err() {
            continue;
        }
        let Some(comm) = parts.next() else {
            continue;
        };
        let args = parts.next().unwrap_or_default();

        if signature == "Claude" && is_ignored_claude_helper_process(comm, args, &helper_pattern) {
            debug!("Ignoring Claude helper process {}:{}", pid_str, comm);
            continue;
        }

        if comm.contains(signature) || args.contains(signature) {
            matches.push(format!("{pid_str}:{comm}"));
        }
    }

    Ok(matches)
}

fn is_ignored_claude_helper_process(comm: &str, args: &str, helper_pattern: &Regex) -> bool {
    helper_pattern.is_match(comm) || helper_pattern.is_match(args)
}

fn create_target_root(temp_parent: Option<&Path>) -> Result<PathBuf> {
    if let Some(parent) = temp_parent {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create temporary parent {}", parent.display()))?;
        let target = parent.join("remote-profile");
        fs::create_dir_all(&target)
            .with_context(|| format!("Failed to create target {}", target.display()))?;
        return Ok(target);
    }

    let target = std::env::temp_dir().join(format!(
        "cowork-remote-profile-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%3f")
    ));
    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create target {}", target.display()))?;
    Ok(target)
}

pub fn build_remote_tar_command(
    remote_profile_path: &str,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    exclude_local_agent_mode_sessions: bool,
) -> Result<String> {
    let profile_expr = remote_path_expression(remote_profile_path)?;
    let mut excludes = Vec::new();

    if !include_vm_bundles {
        excludes.push("--exclude=\"$BASE_NAME/vm_bundles\"".to_string());
        excludes.push("--exclude=\"$BASE_NAME/vm_bundles/*\"".to_string());
    }

    if !include_cache_dirs {
        for cache in NON_ESSENTIAL_CACHE_PATHS {
            excludes.push(format!("--exclude=\"{cache}\""));
        }
    }

    if exclude_local_agent_mode_sessions {
        excludes.push("--exclude=\"$BASE_NAME/local-agent-mode-sessions\"".to_string());
        excludes.push("--exclude=\"$BASE_NAME/local-agent-mode-sessions/*\"".to_string());
    }

    let exclude_expr = if excludes.is_empty() {
        String::new()
    } else {
        format!(" {}", excludes.join(" "))
    };

    Ok(format!(
        "PROFILE_PATH={profile_expr}; if [ ! -d \"$PROFILE_PATH\" ]; then echo \"Remote profile directory does not exist: $PROFILE_PATH\" 1>&2; exit 3; fi; PARENT_DIR=\"$(dirname \"$PROFILE_PATH\")\"; BASE_NAME=\"$(basename \"$PROFILE_PATH\")\"; COPYFILE_DISABLE=1 tar -C \"$PARENT_DIR\" -cf -{exclude_expr} \"$BASE_NAME\""
    ))
}

fn build_remote_tar_from_path_list_command(remote_profile_path: &str) -> Result<String> {
    let profile_expr = remote_path_expression(remote_profile_path)?;
    Ok(format!(
        "PROFILE_PATH={profile_expr}; if [ ! -d \"$PROFILE_PATH\" ]; then echo \"Remote profile directory does not exist: $PROFILE_PATH\" 1>&2; exit 3; fi; cd \"$PROFILE_PATH\"; COPYFILE_DISABLE=1 tar -cf - -T -"
    ))
}

fn build_remote_session_hash_command(
    remote_profile_path: &str,
    hash_algorithm: HashAlgorithm,
) -> Result<String> {
    let profile_expr = remote_path_expression(remote_profile_path)?;
    Ok(format!(
        "PROFILE_PATH={profile_expr}; if [ ! -d \"$PROFILE_PATH\" ]; then echo \"Remote profile directory does not exist: $PROFILE_PATH\" 1>&2; exit 3; fi; cd \"$PROFILE_PATH\"; if [ ! -d \"local-agent-mode-sessions\" ]; then exit 0; fi; if command -v nproc >/dev/null 2>&1; then PARALLELISM=\"$(nproc)\"; elif command -v sysctl >/dev/null 2>&1; then PARALLELISM=\"$(sysctl -n hw.ncpu 2>/dev/null || echo 1)\"; else PARALLELISM=\"1\"; fi; if [ \"$PARALLELISM\" -lt 1 ] 2>/dev/null; then PARALLELISM=1; fi; {}",
        build_remote_hash_xargs_pipeline("\"$PARALLELISM\"", hash_algorithm)
    ))
}

fn build_remote_session_hash_command_with_parallel(
    remote_profile_path: &str,
    parallel_remote: usize,
    hash_algorithm: HashAlgorithm,
) -> Result<String> {
    let profile_expr = remote_path_expression(remote_profile_path)?;
    Ok(format!(
        "PROFILE_PATH={profile_expr}; if [ ! -d \"$PROFILE_PATH\" ]; then echo \"Remote profile directory does not exist: $PROFILE_PATH\" 1>&2; exit 3; fi; PARALLELISM={parallel_remote}; if [ \"$PARALLELISM\" -lt 1 ] 2>/dev/null; then PARALLELISM=1; fi; cd \"$PROFILE_PATH\"; if [ ! -d \"local-agent-mode-sessions\" ]; then exit 0; fi; {}",
        build_remote_hash_xargs_pipeline("\"$PARALLELISM\"", hash_algorithm)
    ))
}

fn build_remote_non_session_hash_command(
    remote_profile_path: &str,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    hash_algorithm: HashAlgorithm,
    parallel_remote: Option<usize>,
) -> Result<String> {
    let profile_expr = remote_path_expression(remote_profile_path)?;
    let parallelism_block = match parallel_remote {
        Some(value) => format!("PARALLELISM={value}; "),
        None => "if command -v nproc >/dev/null 2>&1; then PARALLELISM=\"$(nproc)\"; elif command -v sysctl >/dev/null 2>&1; then PARALLELISM=\"$(sysctl -n hw.ncpu 2>/dev/null || echo 1)\"; else PARALLELISM=\"1\"; fi; ".to_string(),
    };

    let prune_paths = build_prune_paths(include_vm_bundles, include_cache_dirs, true);
    let find_expr = if prune_paths.is_empty() {
        "find . -type f -print0".to_string()
    } else {
        let prune_expr = prune_paths
            .iter()
            .map(|path| format!("-path {}", shell_quote(path)))
            .collect::<Vec<_>>()
            .join(" -o ");
        format!("find . \\( {prune_expr} \\) -prune -o -type f -print0")
    };

    Ok(format!(
        "PROFILE_PATH={profile_expr}; if [ ! -d \"$PROFILE_PATH\" ]; then echo \"Remote profile directory does not exist: $PROFILE_PATH\" 1>&2; exit 3; fi; {parallelism_block}if [ \"$PARALLELISM\" -lt 1 ] 2>/dev/null; then PARALLELISM=1; fi; cd \"$PROFILE_PATH\"; {find_expr} | xargs -0 -P \"$PARALLELISM\" -n {REMOTE_HASH_XARGS_BATCH_SIZE} sh -c 'for file in \"$@\"; do hash=\"$(shasum -a {} \"$file\" | cut -d \" \" -f 1)\"; clean=\"${{file#./}}\"; printf \"%s\\t%s\\n\" \"$clean\" \"$hash\"; done' sh",
        hash_algorithm.shasum_bits()
    ))
}

fn build_remote_hash_xargs_pipeline(
    parallelism_expr: &str,
    hash_algorithm: HashAlgorithm,
) -> String {
    format!(
        "find local-agent-mode-sessions -type f -name 'local_*.json' -print0 | xargs -0 -P {parallelism_expr} -n {REMOTE_HASH_XARGS_BATCH_SIZE} sh -c 'for file in \"$@\"; do hash=\"$(shasum -a {} \"$file\" | cut -d \" \" -f 1)\"; printf \"%s\\t%s\\n\" \"$file\" \"$hash\"; done' sh",
        hash_algorithm.shasum_bits()
    )
}

fn build_prune_paths(
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    exclude_local_agent_mode_sessions: bool,
) -> Vec<String> {
    let mut paths = Vec::new();
    if !include_vm_bundles {
        paths.push("./vm_bundles".to_string());
    }
    if !include_cache_dirs {
        for cache in NON_ESSENTIAL_CACHE_DIRS {
            paths.push(format!("./{cache}"));
        }
    }
    if exclude_local_agent_mode_sessions {
        paths.push("./local-agent-mode-sessions".to_string());
    }
    paths
}

fn list_remote_session_json_hashes(
    remote_host: &str,
    remote_profile_path: &str,
    hash_algorithm: HashAlgorithm,
    parallel_remote: Option<usize>,
) -> Result<HashMap<String, String>> {
    if let Some(value) = parallel_remote {
        if value < 1 {
            bail!("parallel_remote must be >= 1, got {value}");
        }
    }

    let command = match parallel_remote {
        Some(value) => {
            info!(
                "Computing remote session hashes with explicit parallelism={} hash_algorithm={}",
                value,
                hash_algorithm.as_name()
            );
            build_remote_session_hash_command_with_parallel(
                remote_profile_path,
                value,
                hash_algorithm,
            )?
        }
        None => {
            info!(
                "Computing remote session hashes with remote CPU-count parallelism hash_algorithm={}",
                hash_algorithm.as_name()
            );
            build_remote_session_hash_command(remote_profile_path, hash_algorithm)?
        }
    };

    let completed = run_remote_ssh_output_with_spinner(
        remote_host,
        &command,
        "Remote session hash scan",
        &format!(
            "hashing remote session files (algo={}, remote_parallel={})",
            hash_algorithm.as_name(),
            format_remote_parallel(parallel_remote)
        ),
        "scan complete",
        ProgressColor::Magenta,
    )?;

    if !completed.status.success() {
        bail!(
            "Failed to list remote session hashes: {}",
            String::from_utf8_lossy(&completed.stderr).trim()
        );
    }

    parse_remote_hash_lines(&completed.stdout)
}

fn list_remote_non_session_file_hashes(
    remote_host: &str,
    remote_profile_path: &str,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
    hash_algorithm: HashAlgorithm,
    parallel_remote: Option<usize>,
) -> Result<HashMap<String, String>> {
    if let Some(value) = parallel_remote {
        if value < 1 {
            bail!("parallel_remote must be >= 1, got {value}");
        }
    }

    let command = build_remote_non_session_hash_command(
        remote_profile_path,
        include_vm_bundles,
        include_cache_dirs,
        hash_algorithm,
        parallel_remote,
    )?;

    let completed = run_remote_ssh_output_with_spinner(
        remote_host,
        &command,
        "Remote base hash scan",
        &format!(
            "hashing remote non-session files (algo={}, remote_parallel={})",
            hash_algorithm.as_name(),
            format_remote_parallel(parallel_remote)
        ),
        "scan complete",
        ProgressColor::Yellow,
    )?;

    if !completed.status.success() {
        bail!(
            "Failed to list remote base-file hashes: {}",
            String::from_utf8_lossy(&completed.stderr).trim()
        );
    }

    parse_remote_hash_lines(&completed.stdout)
}

fn parse_remote_hash_lines(stdout: &[u8]) -> Result<HashMap<String, String>> {
    let mut hashes = HashMap::new();
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((relative_path, file_hash)) = trimmed.split_once('\t') else {
            warn!("Skipping malformed remote hash line: {}", trimmed);
            continue;
        };
        let relative_path = relative_path.trim();
        let file_hash = file_hash.trim();
        if relative_path.is_empty() || file_hash.is_empty() {
            continue;
        }
        hashes.insert(relative_path.to_string(), file_hash.to_string());
    }
    Ok(hashes)
}

fn run_remote_ssh_output_with_spinner(
    remote_host: &str,
    command: &str,
    label: &str,
    detail: &str,
    success_detail: &str,
    color: ProgressColor,
) -> Result<std::process::Output> {
    run_with_spinner_result(label, detail, color, success_detail, || {
        Command::new("ssh")
            .arg(remote_host)
            .arg(command)
            .output()
            .with_context(|| format!("Failed to run ssh command for {remote_host}"))
    })
}

fn format_remote_parallel(parallel_remote: Option<usize>) -> String {
    match parallel_remote {
        Some(value) => value.to_string(),
        None => "auto(remote CPUs)".to_string(),
    }
}

#[cfg(test)]
fn paths_to_transfer_for_remote_sessions(
    remote_hashes: &HashMap<String, String>,
    baseline_profile: &Path,
    progress: Option<&mut TerminalProgress>,
) -> Vec<String> {
    let cache_root = baseline_profile.join(".__cowork_missing_session_cache__");
    let plan = plan_remote_session_sync(
        remote_hashes,
        baseline_profile,
        &cache_root,
        HashAlgorithm::Sha256,
        1,
        progress,
    );
    session_entries_to_path_list(&plan.transfer_entries)
}

fn plan_remote_session_sync(
    remote_hashes: &HashMap<String, String>,
    baseline_profile: &Path,
    session_cache_root: &Path,
    hash_algorithm: HashAlgorithm,
    parallel_local: usize,
    mut progress: Option<&mut TerminalProgress>,
) -> SessionSyncPlan {
    enum SessionDecision {
        Ignore,
        SeedFromCache((String, String)),
        Transfer((String, String)),
    }

    let sorted_paths: Vec<String> = remote_hashes
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let hash_pool = build_local_hash_pool(parallel_local);
    let decisions: Vec<SessionDecision> = hash_pool.install(|| {
        sorted_paths
            .par_iter()
            .map(|relative_json| {
                if !relative_json.ends_with(".json") {
                    return SessionDecision::Ignore;
                }
                let session_folder = relative_json.trim_end_matches(".json").to_string();
                let remote_hash = remote_hashes
                    .get(relative_json)
                    .map(String::as_str)
                    .unwrap_or_default();
                let local_json = baseline_profile.join(relative_json);
                if should_transfer_remote_session_json(&local_json, remote_hash, hash_algorithm) {
                    if session_cache_has_entry(
                        session_cache_root,
                        relative_json,
                        &session_folder,
                        remote_hash,
                        hash_algorithm,
                    ) {
                        SessionDecision::SeedFromCache((relative_json.clone(), session_folder))
                    } else {
                        SessionDecision::Transfer((relative_json.clone(), session_folder))
                    }
                } else {
                    SessionDecision::Ignore
                }
            })
            .collect()
    });

    let mut plan = SessionSyncPlan::default();
    for (index, decision) in decisions.into_iter().enumerate() {
        match decision {
            SessionDecision::Ignore => {}
            SessionDecision::SeedFromCache(entry) => plan.seed_from_cache_entries.push(entry),
            SessionDecision::Transfer(entry) => plan.transfer_entries.push(entry),
        }
        if let Some(progress_ref) = progress.as_mut() {
            progress_ref.update(
                (index + 1) as u64,
                &format!(
                    "transfer_sessions={} seed_cache={}",
                    plan.transfer_entries.len(),
                    plan.seed_from_cache_entries.len()
                ),
                false,
            );
        }
    }

    plan
}

fn session_entries_to_path_list(entries: &[(String, String)]) -> Vec<String> {
    let mut paths = Vec::with_capacity(entries.len() * 2);
    for (json_path, folder_path) in entries {
        paths.push(json_path.clone());
        paths.push(folder_path.clone());
    }
    paths
}

fn session_cache_has_entry(
    cache_root: &Path,
    json_path: &str,
    folder_path: &str,
    remote_hash: &str,
    hash_algorithm: HashAlgorithm,
) -> bool {
    local_file_matches_hash(&cache_root.join(json_path), remote_hash, hash_algorithm)
        && cache_root.join(folder_path).is_dir()
}

fn plan_remote_base_sync(
    remote_hashes: &HashMap<String, String>,
    baseline_profile: &Path,
    base_cache_root: &Path,
    hash_algorithm: HashAlgorithm,
    parallel_local: usize,
    mut progress: Option<&mut TerminalProgress>,
) -> BaseSyncPlan {
    enum BaseDecision {
        SeedFromBaseline(String),
        SeedFromCache(String),
        Transfer(String),
    }

    let sorted_paths: Vec<String> = remote_hashes
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let hash_pool = build_local_hash_pool(parallel_local);
    let decisions: Vec<BaseDecision> = hash_pool.install(|| {
        sorted_paths
            .par_iter()
            .map(|relative_path| {
                let remote_hash = remote_hashes
                    .get(relative_path)
                    .map(String::as_str)
                    .unwrap_or_default();
                if local_file_matches_hash(
                    &baseline_profile.join(relative_path),
                    remote_hash,
                    hash_algorithm,
                ) {
                    BaseDecision::SeedFromBaseline(relative_path.clone())
                } else if local_file_matches_hash(
                    &base_cache_root.join(relative_path),
                    remote_hash,
                    hash_algorithm,
                ) {
                    BaseDecision::SeedFromCache(relative_path.clone())
                } else {
                    BaseDecision::Transfer(relative_path.clone())
                }
            })
            .collect()
    });

    let mut plan = BaseSyncPlan::default();
    for (index, decision) in decisions.into_iter().enumerate() {
        match decision {
            BaseDecision::SeedFromBaseline(path) => plan.seed_from_baseline.push(path),
            BaseDecision::SeedFromCache(path) => plan.seed_from_cache.push(path),
            BaseDecision::Transfer(path) => plan.transfer_paths.push(path),
        }
        if let Some(progress_ref) = progress.as_mut() {
            progress_ref.update(
                (index + 1) as u64,
                &format!(
                    "transfer_paths={} seed_local={} seed_cache={}",
                    plan.transfer_paths.len(),
                    plan.seed_from_baseline.len(),
                    plan.seed_from_cache.len()
                ),
                false,
            );
        }
    }

    plan
}

fn build_local_hash_pool(parallel_local: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_local.max(1))
        .build()
        .expect("local hash thread pool should build")
}

fn should_transfer_remote_session_json(
    local_json: &Path,
    remote_hash: &str,
    hash_algorithm: HashAlgorithm,
) -> bool {
    !local_file_matches_hash(local_json, remote_hash, hash_algorithm)
}

fn local_file_matches_hash(
    local_file: &Path,
    remote_hash: &str,
    hash_algorithm: HashAlgorithm,
) -> bool {
    if !local_file.exists() {
        return false;
    }
    match hash_file_with_algorithm(local_file, hash_algorithm) {
        Ok(local_hash) => local_hash == remote_hash,
        Err(_) => false,
    }
}

fn hash_file_with_algorithm(local_file: &Path, hash_algorithm: HashAlgorithm) -> Result<String> {
    match hash_algorithm {
        HashAlgorithm::Sha256 => sha256_file(local_file),
        HashAlgorithm::Sha1 => sha1_file(local_file),
    }
}

fn seed_files_from_source(
    source_root: &Path,
    target_root: &Path,
    relative_paths: &[String],
    progress_label: &str,
) -> Result<()> {
    if relative_paths.is_empty() {
        return Ok(());
    }

    let mut progress = TerminalProgress::new(
        progress_label,
        Some(relative_paths.len() as u64),
        "files",
        ProgressColor::Blue,
    );
    for (index, relative_path) in relative_paths.iter().enumerate() {
        let source = source_root.join(relative_path);
        let target = target_root.join(relative_path);
        copy_file_with_hardlink_fallback(&source, &target)?;
        progress.update((index + 1) as u64, "", false);
    }
    progress.finish(relative_paths.len() as u64, "seeded", true);
    Ok(())
}

fn sync_remote_base_cache(
    cache_root: &Path,
    baseline_profile: &Path,
    incremental_target_root: &Path,
    plan: &BaseSyncPlan,
) -> Result<()> {
    seed_files_from_source(
        baseline_profile,
        cache_root,
        &plan.seed_from_baseline,
        "Base cache (local)",
    )?;
    seed_files_from_source(
        incremental_target_root,
        cache_root,
        &plan.transfer_paths,
        "Base cache (remote)",
    )
}

fn seed_session_entries_from_cache(
    cache_root: &Path,
    target_root: &Path,
    entries: &[(String, String)],
) -> Result<()> {
    sync_session_entries_from_source(cache_root, target_root, entries, "Session seed (cache)")
}

fn sync_remote_session_cache(
    cache_root: &Path,
    incremental_target_root: &Path,
    entries: &[(String, String)],
) -> Result<()> {
    sync_session_entries_from_source(
        incremental_target_root,
        cache_root,
        entries,
        "Session cache (remote)",
    )
}

fn sync_session_entries_from_source(
    source_root: &Path,
    target_root: &Path,
    entries: &[(String, String)],
    progress_label: &str,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    let mut progress = TerminalProgress::new(
        progress_label,
        Some(entries.len() as u64),
        "sessions",
        ProgressColor::Blue,
    );
    for (index, (json_path, folder_path)) in entries.iter().enumerate() {
        let source_json = source_root.join(json_path);
        let target_json = target_root.join(json_path);
        copy_file_with_hardlink_fallback(&source_json, &target_json)?;

        let source_folder = source_root.join(folder_path);
        let target_folder = target_root.join(folder_path);
        copy_tree_preserve_links(&source_folder, &target_folder)?;
        progress.update((index + 1) as u64, "", false);
    }
    progress.finish(entries.len() as u64, "seeded", true);
    Ok(())
}

fn copy_file_with_hardlink_fallback(source: &Path, target: &Path) -> Result<()> {
    let parent = target.parent().with_context(|| {
        format!(
            "Target path has no parent for file copy {}",
            target.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
    remove_path_if_exists(target)?;
    if fs::hard_link(source, target).is_err() {
        fs::copy(source, target).with_context(|| {
            format!(
                "Failed to copy file {} to {}",
                source.display(),
                target.display()
            )
        })?;
    }
    Ok(())
}

fn copy_tree_preserve_links(source_root: &Path, target_root: &Path) -> Result<()> {
    let source_metadata = fs::symlink_metadata(source_root)
        .with_context(|| format!("Failed to stat source tree {}", source_root.display()))?;
    if !source_metadata.file_type().is_dir() {
        bail!(
            "Expected directory when seeding session tree: {}",
            source_root.display()
        );
    }

    fs::create_dir_all(target_root)
        .with_context(|| format!("Failed to create {}", target_root.display()))?;
    for entry in fs::read_dir(source_root)
        .with_context(|| format!("Failed to read directory {}", source_root.display()))?
    {
        let entry = entry
            .with_context(|| format!("Failed reading entry under {}", source_root.display()))?;
        let source_path = entry.path();
        let target_path = target_root.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .with_context(|| format!("Failed to stat {}", source_path.display()))?;

        if metadata.file_type().is_dir() {
            copy_tree_preserve_links(&source_path, &target_path)?;
            continue;
        }

        if metadata.file_type().is_symlink() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let link_target = fs::read_link(&source_path)
                    .with_context(|| format!("Failed to read symlink {}", source_path.display()))?;
                remove_path_if_exists(&target_path)?;
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create {}", parent.display()))?;
                }
                symlink(&link_target, &target_path).with_context(|| {
                    format!(
                        "Failed to seed symlink {} -> {}",
                        source_path.display(),
                        target_path.display()
                    )
                })?;
            }
            #[cfg(not(unix))]
            {
                bail!("Symlink seeding is not supported on this platform");
            }
            continue;
        }

        if metadata.file_type().is_file() {
            copy_file_with_hardlink_fallback(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("Failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn resolve_remote_base_cache_path(remote_host: &str, remote_profile_path: &str) -> Result<PathBuf> {
    resolve_remote_cache_path(remote_host, remote_profile_path, "cowork-remote-base-cache")
}

fn resolve_remote_session_cache_path(
    remote_host: &str,
    remote_profile_path: &str,
) -> Result<PathBuf> {
    resolve_remote_cache_path(
        remote_host,
        remote_profile_path,
        "cowork-remote-session-cache",
    )
}

fn resolve_remote_cache_path(
    remote_host: &str,
    remote_profile_path: &str,
    cache_namespace: &str,
) -> Result<PathBuf> {
    let remote_name = remote_profile_name(remote_profile_path)?;
    let host_key = sanitize_for_path_component(remote_host);
    let profile_hash = sha256_text(remote_profile_path)
        .chars()
        .take(12)
        .collect::<String>();
    Ok(std::env::temp_dir()
        .join(cache_namespace)
        .join(host_key)
        .join(format!("{remote_name}-{profile_hash}")))
}

fn sanitize_for_path_component(value: &str) -> String {
    let mut rendered = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphanumeric()
            || character == '-'
            || character == '_'
            || character == '.'
        {
            rendered.push(character);
        } else {
            rendered.push('_');
        }
    }
    if rendered.is_empty() {
        "remote".to_string()
    } else {
        rendered
    }
}

fn remote_path_expression(remote_profile_path: &str) -> Result<String> {
    let normalized = remote_profile_path.trim();
    if normalized.is_empty() {
        bail!("Remote profile path must be non-empty");
    }

    if normalized.starts_with('/') {
        return Ok(shell_quote(normalized));
    }

    let stripped = normalized.trim_start_matches('/');
    Ok(format!("$HOME/{}", shell_quote(stripped)))
}

fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn remote_profile_name(remote_profile_path: &str) -> Result<String> {
    let trimmed = remote_profile_path.trim_end_matches('/');
    let Some(name) = trimmed.rsplit('/').find(|part| !part.is_empty()) else {
        bail!("Invalid remote profile path: {remote_profile_path}");
    };
    Ok(name.to_string())
}

fn fetch_remote_tar_with_command(
    remote_host: &str,
    command: &str,
    target_root: &Path,
    progress_label: &str,
) -> Result<()> {
    let ssh_process = Command::new("ssh")
        .arg(remote_host)
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start ssh transfer from {remote_host}"))?;

    extract_remote_tar_stream(ssh_process, target_root, progress_label)
}

fn fetch_remote_tar_with_path_list(
    remote_host: &str,
    command: &str,
    target_root: &Path,
    relative_paths: &[String],
    progress_label: &str,
) -> Result<()> {
    if relative_paths.is_empty() {
        return Ok(());
    }

    let mut ssh_process = Command::new("ssh")
        .arg(remote_host)
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start ssh path-list transfer from {remote_host}"))?;

    let payload = format!("{}\n", relative_paths.join("\n"));
    let mut stdin = ssh_process
        .stdin
        .take()
        .with_context(|| "Failed to capture ssh stdin for path-list transfer")?;
    stdin
        .write_all(payload.as_bytes())
        .with_context(|| "Failed to send remote path list")?;
    stdin
        .flush()
        .with_context(|| "Failed to flush remote path-list payload")?;
    drop(stdin);

    extract_remote_tar_stream(ssh_process, target_root, progress_label)
}

fn extract_remote_tar_stream(
    mut ssh_process: Child,
    target_root: &Path,
    progress_label: &str,
) -> Result<()> {
    let mut tar_process = Command::new("tar")
        .arg("-xf")
        .arg("-")
        .arg("-C")
        .arg(target_root)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "Failed to start local tar extraction into {}",
                target_root.display()
            )
        })?;

    let mut progress = TerminalProgress::new(progress_label, None, "bytes", ProgressColor::Cyan)
        .with_formatter(format_bytes);
    let mut transferred = 0_u64;

    {
        let mut ssh_stdout = ssh_process
            .stdout
            .take()
            .with_context(|| "Failed to capture ssh stdout")?;
        let mut tar_stdin = tar_process
            .stdin
            .take()
            .with_context(|| "Failed to capture tar stdin")?;

        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let bytes_read = ssh_stdout
                .read(&mut buffer)
                .with_context(|| "Failed reading remote tar payload")?;
            if bytes_read == 0 {
                break;
            }
            tar_stdin
                .write_all(&buffer[..bytes_read])
                .with_context(|| "Failed writing tar payload to extractor")?;
            transferred += bytes_read as u64;
            progress.update(transferred, "", false);
        }
        tar_stdin
            .flush()
            .with_context(|| "Failed to flush tar extractor stdin")?;
    }

    let tar_status = tar_process
        .wait()
        .with_context(|| "Failed waiting for local tar extraction")?;
    let ssh_status = ssh_process
        .wait()
        .with_context(|| "Failed waiting for ssh transfer")?;

    let tar_stderr = read_stderr(&mut tar_process)?;
    let ssh_stderr = read_stderr(&mut ssh_process)?;

    if !ssh_status.success() {
        error!("SSH transfer failed: {}", ssh_stderr.trim());
        progress.finish(
            transferred,
            &format!("ssh_exit={:?}", ssh_status.code()),
            false,
        );
        bail!(
            "SSH transfer failed (exit {:?}): {}",
            ssh_status.code(),
            ssh_stderr.trim()
        );
    }

    if !tar_status.success() {
        error!("Tar extraction failed: {}", tar_stderr.trim());
        progress.finish(
            transferred,
            &format!("tar_exit={:?}", tar_status.code()),
            false,
        );
        bail!(
            "Failed to extract remote profile stream (exit {:?}): {}",
            tar_status.code(),
            tar_stderr.trim()
        );
    }

    progress.finish(transferred, "transferred", true);
    Ok(())
}

fn read_stderr(process: &mut std::process::Child) -> Result<String> {
    let mut stderr = String::new();
    if let Some(mut handle) = process.stderr.take() {
        handle
            .read_to_string(&mut stderr)
            .with_context(|| "Failed to read child stderr")?;
    }
    Ok(stderr)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::utils::{sha1_text, sha256_text};

    use super::{
        build_remote_session_hash_command, build_remote_session_hash_command_with_parallel,
        build_remote_tar_command, paths_to_transfer_for_remote_sessions, plan_remote_base_sync,
        plan_remote_session_sync, should_transfer_remote_session_json, HashAlgorithm,
    };

    #[test]
    fn build_remote_tar_command_excludes_vm_bundles_by_default() {
        let command =
            build_remote_tar_command("Library/Application Support/Claude", false, false, false)
                .expect("command should build");
        assert!(command.contains("--exclude=\"$BASE_NAME/vm_bundles\""));
        assert!(command.contains("--exclude=\"$BASE_NAME/vm_bundles/*\""));
        assert!(command.contains("COPYFILE_DISABLE=1 tar"));
    }

    #[test]
    fn build_remote_tar_command_can_include_vm_bundles() {
        let command =
            build_remote_tar_command("Library/Application Support/Claude", true, false, false)
                .expect("command should build");
        assert!(!command.contains("vm_bundles"));
        assert!(command.contains("COPYFILE_DISABLE=1 tar"));
    }

    #[test]
    fn build_remote_tar_command_excludes_caches_by_default() {
        let command =
            build_remote_tar_command("Library/Application Support/Claude", true, false, false)
                .expect("command should build");
        assert!(command.contains("--exclude=\"$BASE_NAME/Cache\""));
        assert!(command.contains("--exclude=\"$BASE_NAME/Code Cache\""));
        assert!(command.contains("--exclude=\"$BASE_NAME/Service Worker/CacheStorage\""));
    }

    #[test]
    fn build_remote_tar_command_can_include_caches() {
        let command =
            build_remote_tar_command("Library/Application Support/Claude", true, true, false)
                .expect("command should build");
        assert!(!command.contains("--exclude=\"$BASE_NAME/Cache\""));
        assert!(!command.contains("--exclude=\"$BASE_NAME/Code Cache\""));
    }

    #[test]
    fn build_remote_session_hash_command_defaults_to_remote_cores() {
        let command = build_remote_session_hash_command(
            "Library/Application Support/Claude",
            HashAlgorithm::Sha256,
        )
        .expect("command should build");
        assert!(command.contains("command -v nproc"));
        assert!(command.contains("sysctl -n hw.ncpu"));
        assert!(command.contains("xargs -0 -P \"$PARALLELISM\" -n 32"));
        assert!(command.contains("shasum -a 256"));
    }

    #[test]
    fn build_remote_session_hash_command_with_parallel_uses_explicit_limit() {
        let command = build_remote_session_hash_command_with_parallel(
            "Library/Application Support/Claude",
            7,
            HashAlgorithm::Sha256,
        )
        .expect("command should build");
        assert!(command.contains("PARALLELISM=7"));
        assert!(command.contains("xargs -0 -P \"$PARALLELISM\" -n 32"));
        assert!(command.contains("shasum -a 256"));
    }

    #[test]
    fn build_remote_session_hash_command_supports_sha1() {
        let command = build_remote_session_hash_command(
            "Library/Application Support/Claude",
            HashAlgorithm::Sha1,
        )
        .expect("command should build");
        assert!(command.contains("shasum -a 1"));
    }

    #[test]
    fn paths_to_transfer_for_remote_sessions_detects_changed_and_missing() {
        let tmp = tempdir().expect("tempdir");
        let baseline = tmp.path().join("baseline");

        let same_json = baseline.join("local-agent-mode-sessions/u/o/local_same.json");
        let changed_json = baseline.join("local-agent-mode-sessions/u/o/local_changed.json");
        fs::create_dir_all(
            same_json
                .parent()
                .expect("local session file should have parent"),
        )
        .expect("create baseline dirs");
        fs::write(&same_json, "same").expect("write same json");
        fs::write(&changed_json, "old").expect("write changed json");

        let hashes = HashMap::from([
            (
                "local-agent-mode-sessions/u/o/local_same.json".to_string(),
                sha256_text("same"),
            ),
            (
                "local-agent-mode-sessions/u/o/local_changed.json".to_string(),
                sha256_text("new"),
            ),
            (
                "local-agent-mode-sessions/u/o/local_missing.json".to_string(),
                sha256_text("missing"),
            ),
        ]);

        let paths = paths_to_transfer_for_remote_sessions(&hashes, &baseline, None);

        assert!(!paths.contains(&"local-agent-mode-sessions/u/o/local_same.json".to_string()));
        assert!(paths.contains(&"local-agent-mode-sessions/u/o/local_changed.json".to_string()));
        assert!(paths.contains(&"local-agent-mode-sessions/u/o/local_changed".to_string()));
        assert!(paths.contains(&"local-agent-mode-sessions/u/o/local_missing.json".to_string()));
        assert!(paths.contains(&"local-agent-mode-sessions/u/o/local_missing".to_string()));
    }

    #[test]
    fn should_transfer_remote_session_json_handles_existing_match() {
        let tmp = tempdir().expect("tempdir");
        let local_json = tmp
            .path()
            .join("local-agent-mode-sessions/u/o/local_same.json");
        fs::create_dir_all(
            local_json
                .parent()
                .expect("local session file should have parent"),
        )
        .expect("create dirs");
        fs::write(&local_json, "same").expect("write local json");
        assert!(!should_transfer_remote_session_json(
            &local_json,
            &sha256_text("same"),
            HashAlgorithm::Sha256,
        ));
        assert!(!should_transfer_remote_session_json(
            &local_json,
            &sha1_text("same"),
            HashAlgorithm::Sha1,
        ));
    }

    #[test]
    fn plan_remote_base_sync_uses_baseline_then_cache_then_transfer() {
        let tmp = tempdir().expect("tempdir");
        let baseline = tmp.path().join("baseline");
        let cache = tmp.path().join("cache");
        fs::create_dir_all(&baseline).expect("create baseline root");
        fs::create_dir_all(&cache).expect("create cache root");

        let baseline_same = baseline.join("Local Storage/leveldb/CURRENT");
        let baseline_changed = baseline.join("Local Storage/leveldb/LOG");
        let cache_same = cache.join("Local Storage/leveldb/LOG");

        fs::create_dir_all(
            baseline_same
                .parent()
                .expect("baseline file should have parent"),
        )
        .expect("create baseline parent");
        fs::create_dir_all(cache_same.parent().expect("cache file should have parent"))
            .expect("create cache parent");

        fs::write(&baseline_same, "same").expect("write baseline same");
        fs::write(&baseline_changed, "old").expect("write baseline changed");
        fs::write(&cache_same, "remote-log").expect("write cache same");

        let hashes = HashMap::from([
            (
                "Local Storage/leveldb/CURRENT".to_string(),
                sha256_text("same"),
            ),
            (
                "Local Storage/leveldb/LOG".to_string(),
                sha256_text("remote-log"),
            ),
            (
                "Local Storage/leveldb/MANIFEST-000001".to_string(),
                sha256_text("remote-new"),
            ),
        ]);

        let plan =
            plan_remote_base_sync(&hashes, &baseline, &cache, HashAlgorithm::Sha256, 1, None);

        assert_eq!(plan.seed_from_baseline.len(), 1);
        assert_eq!(plan.seed_from_cache.len(), 1);
        assert_eq!(plan.transfer_paths.len(), 1);
        assert!(plan
            .seed_from_baseline
            .contains(&"Local Storage/leveldb/CURRENT".to_string()));
        assert!(plan
            .seed_from_cache
            .contains(&"Local Storage/leveldb/LOG".to_string()));
        assert!(plan
            .transfer_paths
            .contains(&"Local Storage/leveldb/MANIFEST-000001".to_string()));
    }

    #[test]
    fn plan_remote_session_sync_uses_cache_for_mismatched_local_sessions() {
        let tmp = tempdir().expect("tempdir");
        let baseline = tmp.path().join("baseline");
        let cache = tmp.path().join("cache");

        let baseline_same = baseline.join("local-agent-mode-sessions/u/o/local_same.json");
        let baseline_changed = baseline.join("local-agent-mode-sessions/u/o/local_changed.json");
        let cache_changed_json = cache.join("local-agent-mode-sessions/u/o/local_changed.json");
        let cache_changed_folder = cache.join("local-agent-mode-sessions/u/o/local_changed");

        fs::create_dir_all(
            baseline_same
                .parent()
                .expect("baseline session file should have parent"),
        )
        .expect("create baseline parent");
        fs::create_dir_all(
            cache_changed_json
                .parent()
                .expect("cache session file should have parent"),
        )
        .expect("create cache parent");
        fs::create_dir_all(&cache_changed_folder).expect("create cache folder");

        fs::write(&baseline_same, "same").expect("write baseline same");
        fs::write(&baseline_changed, "old").expect("write baseline changed");
        fs::write(&cache_changed_json, "remote-changed").expect("write cache changed");

        let hashes = HashMap::from([
            (
                "local-agent-mode-sessions/u/o/local_same.json".to_string(),
                sha256_text("same"),
            ),
            (
                "local-agent-mode-sessions/u/o/local_changed.json".to_string(),
                sha256_text("remote-changed"),
            ),
            (
                "local-agent-mode-sessions/u/o/local_missing.json".to_string(),
                sha256_text("remote-missing"),
            ),
        ]);

        let plan =
            plan_remote_session_sync(&hashes, &baseline, &cache, HashAlgorithm::Sha256, 1, None);

        assert_eq!(plan.seed_from_cache_entries.len(), 1);
        assert_eq!(plan.transfer_entries.len(), 1);
        assert_eq!(
            plan.seed_from_cache_entries[0].0,
            "local-agent-mode-sessions/u/o/local_changed.json"
        );
        assert_eq!(
            plan.transfer_entries[0].0,
            "local-agent-mode-sessions/u/o/local_missing.json"
        );
    }

    use std::collections::HashMap;
}
