use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use log::{debug, error, info};
use regex::Regex;
use which::which;

use crate::progress::{format_bytes, ProgressColor, TerminalProgress};

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

pub fn fetch_remote_profile(
    remote_host: &str,
    remote_profile_path: &str,
    temp_parent: Option<&Path>,
    include_vm_bundles: bool,
    include_cache_dirs: bool,
) -> Result<PathBuf> {
    if remote_host.trim().is_empty() {
        bail!("Remote host must be a non-empty string");
    }

    ensure_ssh_available()?;
    ensure_remote_claude_not_running(remote_host)?;

    let target_root = create_target_root(temp_parent)?;
    let command =
        build_remote_tar_command(remote_profile_path, include_vm_bundles, include_cache_dirs)?;

    info!(
        "Fetching remote profile from {}:{}",
        remote_host, remote_profile_path
    );
    fetch_remote_tar_with_command(remote_host, &command, &target_root)?;

    let fetched_path = target_root.join(remote_profile_name(remote_profile_path)?);
    if !fetched_path.exists() {
        bail!(
            "Fetched profile not found after transfer: {}",
            fetched_path.display()
        );
    }

    Ok(fetched_path)
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
    let completed = Command::new("ssh")
        .arg(remote_host)
        .arg("ps -axo pid=,comm=,args=")
        .output()
        .with_context(|| format!("Failed to run process check via ssh for {remote_host}"))?;

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

    let exclude_expr = if excludes.is_empty() {
        String::new()
    } else {
        format!(" {}", excludes.join(" "))
    };

    Ok(format!(
        "PROFILE_PATH={profile_expr}; if [ ! -d \"$PROFILE_PATH\" ]; then echo \"Remote profile directory does not exist: $PROFILE_PATH\" 1>&2; exit 3; fi; PARENT_DIR=\"$(dirname \"$PROFILE_PATH\")\"; BASE_NAME=\"$(basename \"$PROFILE_PATH\")\"; COPYFILE_DISABLE=1 tar -C \"$PARENT_DIR\" -cf -{exclude_expr} \"$BASE_NAME\""
    ))
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
) -> Result<()> {
    let mut ssh_process = Command::new("ssh")
        .arg(remote_host)
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start ssh transfer from {remote_host}"))?;

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

    let mut progress = TerminalProgress::new("Remote fetch", None, "bytes", ProgressColor::Cyan)
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
    use super::build_remote_tar_command;

    #[test]
    fn build_remote_tar_command_excludes_vm_bundles_by_default() {
        let command = build_remote_tar_command("Library/Application Support/Claude", false, false)
            .expect("command should build");
        assert!(command.contains("--exclude=\"$BASE_NAME/vm_bundles\""));
        assert!(command.contains("--exclude=\"$BASE_NAME/vm_bundles/*\""));
        assert!(command.contains("COPYFILE_DISABLE=1 tar"));
    }

    #[test]
    fn build_remote_tar_command_can_include_vm_bundles() {
        let command = build_remote_tar_command("Library/Application Support/Claude", true, false)
            .expect("command should build");
        assert!(!command.contains("vm_bundles"));
        assert!(command.contains("COPYFILE_DISABLE=1 tar"));
    }

    #[test]
    fn build_remote_tar_command_excludes_caches_by_default() {
        let command = build_remote_tar_command("Library/Application Support/Claude", true, false)
            .expect("command should build");
        assert!(command.contains("--exclude=\"$BASE_NAME/Cache\""));
        assert!(command.contains("--exclude=\"$BASE_NAME/Code Cache\""));
        assert!(command.contains("--exclude=\"$BASE_NAME/Service Worker/CacheStorage\""));
    }

    #[test]
    fn build_remote_tar_command_can_include_caches() {
        let command = build_remote_tar_command("Library/Application Support/Claude", true, true)
            .expect("command should build");
        assert!(!command.contains("--exclude=\"$BASE_NAME/Cache\""));
        assert!(!command.contains("--exclude=\"$BASE_NAME/Code Cache\""));
    }
}
