use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand};
use log::error;
use regex::Regex;
use serde::Serialize;
use tempfile::TempDir;

use crate::deploy::atomic_swap_profile;
use crate::merge_engine::{merge_profiles, MergeOptions, MergeSummary};
use crate::remote_profile::fetch_remote_profile;

#[derive(Debug, Parser)]
#[command(
    name = "cowork-merge-rs",
    about = "Offline Claude Cowork profile merge tool (Rust)."
)]
struct Cli {
    #[arg(
        long,
        default_value = "WARNING",
        help = "Logging level (DEBUG, INFO, WARNING, ERROR)."
    )]
    log_level: String,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Merge two profile directories into one output profile.")]
    Merge(MergeArgs),
    #[command(about = "Atomically swap merged profile into live path.")]
    Deploy(DeployArgs),
}

#[derive(Debug, clap::Args)]
struct MergeArgs {
    #[arg(long = "profile-a")]
    profile_a: Option<PathBuf>,
    #[arg(long = "profile-b")]
    profile_b: Option<PathBuf>,
    #[arg(long = "merge-from")]
    merge_from: Option<String>,
    #[arg(
        long = "remote-profile-path",
        default_value = "Library/Application Support/Claude",
        help = "Remote profile path (absolute, or relative to remote home directory)."
    )]
    remote_profile_path: String,
    #[arg(long = "output-profile")]
    output_profile: Option<PathBuf>,
    #[arg(long = "browser-state-a")]
    browser_state_a: Option<PathBuf>,
    #[arg(long = "browser-state-b")]
    browser_state_b: Option<PathBuf>,
    #[arg(long = "browser-state-output")]
    browser_state_output: Option<PathBuf>,
    #[arg(long = "base-source", default_value = "a", value_parser = ["a", "b"])]
    base_source: String,
    #[arg(long = "skip-browser-state", action = ArgAction::SetTrue)]
    skip_browser_state: bool,
    #[arg(long = "skip-indexeddb", action = ArgAction::SetTrue)]
    skip_indexeddb: bool,
    #[arg(long = "include-vm-bundles", action = ArgAction::SetTrue)]
    include_vm_bundles: bool,
    #[arg(long = "include-cache-dirs", action = ArgAction::SetTrue)]
    include_cache_dirs: bool,
    #[arg(long = "apply", action = ArgAction::SetTrue)]
    apply: bool,
    #[arg(long = "force", action = ArgAction::SetTrue)]
    force: bool,
    #[arg(long = "include-sensitive-claude-credentials", action = ArgAction::SetTrue)]
    include_sensitive_claude_credentials: bool,
}

#[derive(Debug, clap::Args)]
struct DeployArgs {
    #[arg(long = "live-profile")]
    live_profile: PathBuf,
    #[arg(long = "merged-profile")]
    merged_profile: PathBuf,
    #[arg(long = "backup-parent")]
    backup_parent: PathBuf,
}

#[derive(Debug, Serialize)]
struct MergeResultPayload {
    #[serde(rename = "outputProfile")]
    output_profile: String,
    #[serde(rename = "mergedSessionCount")]
    merged_session_count: usize,
    #[serde(rename = "browserStateOutput")]
    browser_state_output: Option<String>,
    applied: bool,
    #[serde(rename = "liveProfile")]
    live_profile: Option<String>,
    #[serde(rename = "backupProfile")]
    backup_profile: Option<String>,
    validation: ValidationPayload,
}

#[derive(Debug, Serialize)]
struct ValidationPayload {
    #[serde(rename = "isValid")]
    is_valid: bool,
    #[serde(rename = "missingSessionFolders")]
    missing_session_folders: Vec<String>,
    #[serde(rename = "missingCliBindingKeys")]
    missing_cli_binding_keys: Vec<String>,
    #[serde(rename = "missingCoworkReadStateSessions")]
    missing_cowork_read_state_sessions: Vec<String>,
}

pub fn run<I, T>(argv: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(argv);
    configure_logging(&cli.log_level);

    match cli.command {
        Commands::Merge(args) => run_merge(args),
        Commands::Deploy(args) => run_deploy(args),
    }
}

fn configure_logging(level_name: &str) {
    let level = match level_name.to_ascii_uppercase().as_str() {
        "DEBUG" => log::LevelFilter::Debug,
        "INFO" => log::LevelFilter::Info,
        "WARNING" => log::LevelFilter::Warn,
        "ERROR" => log::LevelFilter::Error,
        _ => log::LevelFilter::Warn,
    };

    let mut builder = env_logger::Builder::from_default_env();
    builder.filter_level(level);
    let _ = builder.try_init();
}

fn run_merge(args: MergeArgs) -> Result<()> {
    let profile_a = args
        .profile_a
        .clone()
        .unwrap_or_else(default_local_profile_path);
    let output_profile = args
        .output_profile
        .clone()
        .unwrap_or_else(default_output_profile_path);

    let mut remote_tempdir: Option<TempDir> = None;
    let profile_b = resolve_profile_b(&args, &mut remote_tempdir)?;

    let (browser_state_a, browser_state_b, browser_state_output) =
        resolve_browser_state_paths(&args)?;

    let options = MergeOptions {
        profile_a: profile_a.clone(),
        profile_b,
        output_profile,
        include_sensitive_claude_credentials: args.include_sensitive_claude_credentials,
        base_source: args.base_source.clone(),
        browser_state_a_path: browser_state_a,
        browser_state_b_path: browser_state_b,
        browser_state_output_path: browser_state_output,
        merge_indexeddb: !args.skip_indexeddb,
        skip_browser_state: args.skip_browser_state,
        force_output_overwrite: args.force,
        include_vm_bundles: args.include_vm_bundles,
        include_cache_dirs: args.include_cache_dirs,
    };

    let summary = merge_profiles(&options)?;
    let backup_profile = apply_merged_profile_if_requested(&args, &summary, &profile_a)?;

    let payload = MergeResultPayload {
        output_profile: summary.output_profile.display().to_string(),
        merged_session_count: summary.merged_session_count,
        browser_state_output: summary
            .browser_state_output
            .as_ref()
            .map(|path| path.display().to_string()),
        applied: args.apply,
        live_profile: if args.apply {
            Some(profile_a.display().to_string())
        } else {
            None
        },
        backup_profile: backup_profile.map(|path| path.display().to_string()),
        validation: ValidationPayload {
            is_valid: summary.validation.is_valid(),
            missing_session_folders: summary.validation.missing_session_folders,
            missing_cli_binding_keys: summary.validation.missing_cli_binding_keys,
            missing_cowork_read_state_sessions: summary
                .validation
                .missing_cowork_read_state_sessions,
        },
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .with_context(|| "Failed to render merge result JSON")?
    );

    Ok(())
}

fn run_deploy(args: DeployArgs) -> Result<()> {
    let backup = atomic_swap_profile(
        &args.live_profile,
        &args.merged_profile,
        &args.backup_parent,
    )?;
    let payload = serde_json::json!({
        "liveProfile": args.live_profile,
        "mergedProfile": args.merged_profile,
        "backupProfile": backup,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).with_context(|| "Failed to render deploy result")?
    );
    Ok(())
}

fn resolve_profile_b(args: &MergeArgs, remote_tempdir: &mut Option<TempDir>) -> Result<PathBuf> {
    if args.profile_b.is_some() && args.merge_from.is_some() {
        error!("Use either --profile-b or --merge-from, not both.");
        bail!("Use either --profile-b or --merge-from, not both.");
    }

    if let Some(remote_host) = &args.merge_from {
        let tempdir = tempfile::tempdir()
            .with_context(|| "Failed to create temporary directory for remote profile")?;
        let fetched = fetch_remote_profile(
            remote_host,
            &args.remote_profile_path,
            Some(tempdir.path()),
            args.include_vm_bundles,
            args.include_cache_dirs,
        )?;
        *remote_tempdir = Some(tempdir);
        return Ok(fetched);
    }

    if let Some(profile_b) = &args.profile_b {
        return Ok(profile_b.clone());
    }

    error!("Merge requires --profile-b or --merge-from.");
    bail!("Merge requires --profile-b or --merge-from.")
}

fn resolve_browser_state_paths(
    args: &MergeArgs,
) -> Result<(Option<PathBuf>, Option<PathBuf>, Option<PathBuf>)> {
    if args.skip_browser_state {
        return Ok((None, None, None));
    }

    let provided = [
        args.browser_state_a.is_some(),
        args.browser_state_b.is_some(),
        args.browser_state_output.is_some(),
    ];

    if provided.iter().all(|present| *present) {
        return Ok((
            args.browser_state_a.clone(),
            args.browser_state_b.clone(),
            args.browser_state_output.clone(),
        ));
    }

    if provided.iter().any(|present| *present) {
        bail!(
            "Provide all browser state paths or none: --browser-state-a, --browser-state-b, --browser-state-output"
        );
    }

    bail!(
        "Rust merge currently requires explicit browser-state exports when browser merge is enabled. Provide --browser-state-a/--browser-state-b/--browser-state-output or use --skip-browser-state."
    )
}

fn apply_merged_profile_if_requested(
    args: &MergeArgs,
    summary: &MergeSummary,
    profile_a: &Path,
) -> Result<Option<PathBuf>> {
    if !args.apply {
        return Ok(None);
    }

    if !args.skip_browser_state && summary.browser_state_output.is_some() {
        bail!(
            "--apply currently supports filesystem-only deploy in Rust mode. Re-run with --skip-browser-state and apply manually after browser-state import."
        );
    }

    abort_if_claude_running()?;
    let backup_parent = profile_a
        .parent()
        .with_context(|| format!("Profile A path has no parent: {}", profile_a.display()))?;
    let backup = atomic_swap_profile(profile_a, &summary.output_profile, backup_parent)?;
    Ok(Some(backup))
}

fn abort_if_claude_running() -> Result<()> {
    let running = find_processes_with_signature("Claude")?;
    if running.is_empty() {
        return Ok(());
    }

    bail!(
        "Found running Claude process(es). Quit Claude and retry with --apply. Matches: {}",
        running.join(", ")
    )
}

fn find_processes_with_signature(signature: &str) -> Result<Vec<String>> {
    let completed = Command::new("ps")
        .args(["-axo", "pid=,comm=,args="])
        .output()
        .with_context(|| "Failed to enumerate processes with ps")?;

    if !completed.status.success() {
        bail!(
            "Failed to enumerate processes with ps: {}",
            String::from_utf8_lossy(&completed.stderr).trim()
        );
    }

    let current_pid = std::process::id() as i32;
    let helper_pattern = Regex::new(r"Contents/Helpers/.+").expect("valid helper pattern");

    let mut matches = Vec::new();
    for raw_line in String::from_utf8_lossy(&completed.stdout).lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let Ok(pid) = parts[0].parse::<i32>() else {
            continue;
        };
        if pid == current_pid {
            continue;
        }

        let comm = parts[1];
        let args = if parts.len() > 2 {
            line.splitn(3, char::is_whitespace)
                .nth(2)
                .unwrap_or_default()
        } else {
            ""
        };

        if signature == "Claude" && (helper_pattern.is_match(comm) || helper_pattern.is_match(args))
        {
            continue;
        }

        if comm.contains(signature) || args.contains(signature) {
            matches.push(format!("{pid}:{comm}"));
        }
    }

    Ok(matches)
}

fn default_local_profile_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Claude");
    }
    PathBuf::from("Library/Application Support/Claude")
}

fn default_output_profile_path() -> PathBuf {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    std::env::temp_dir().join(format!("claude-cowork-merged-{timestamp}"))
}
