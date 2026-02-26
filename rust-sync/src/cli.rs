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

use crate::browser_storage::{
    ensure_playwright_available, export_browser_state_with_playwright,
    import_browser_state_with_playwright, read_browser_state,
};
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
    #[command(about = "Export logical browser storage via native Rust Playwright.")]
    ExportBrowserState(ExportBrowserStateArgs),
    #[command(about = "Import logical browser state via native Rust Playwright.")]
    ImportBrowserState(ImportBrowserStateArgs),
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
    #[arg(
        long = "auto-export-browser-state",
        action = ArgAction::SetTrue,
        help = "Auto-export browser state for both profiles when browser state files are not provided."
    )]
    auto_export_browser_state: bool,
    #[arg(
        long = "headless-browser-state",
        action = ArgAction::SetTrue,
        help = "Run browser-state export/import in headless mode (default: enabled)."
    )]
    headless_browser_state: bool,
    #[arg(
        long = "no-headless-browser-state",
        action = ArgAction::SetTrue,
        help = "Disable headless mode for browser-state export/import."
    )]
    no_headless_browser_state: bool,
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
struct ExportBrowserStateArgs {
    #[arg(long = "profile")]
    profile: PathBuf,
    #[arg(long = "output")]
    output: PathBuf,
    #[arg(long = "origin", default_value = "https://claude.ai")]
    origin: String,
    #[arg(long = "headless", action = ArgAction::SetTrue)]
    headless: bool,
}

#[derive(Debug, clap::Args)]
struct ImportBrowserStateArgs {
    #[arg(long = "profile")]
    profile: PathBuf,
    #[arg(long = "input")]
    input: PathBuf,
    #[arg(long = "headless", action = ArgAction::SetTrue)]
    headless: bool,
    #[arg(long = "replace-local-storage", action = ArgAction::SetTrue)]
    replace_local_storage: bool,
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
        Commands::ExportBrowserState(args) => run_export_browser_state(args),
        Commands::ImportBrowserState(args) => run_import_browser_state(args),
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
    preflight_browser_state_requirements(&args)?;

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

    let mut browser_state_tempdir: Option<TempDir> = None;
    let (browser_state_a, browser_state_b, browser_state_output) =
        resolve_browser_state_paths(&args, &profile_a, &profile_b, &mut browser_state_tempdir)?;

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
    let backup_profile = apply_merged_profile_if_requested(
        &args,
        &summary,
        &profile_a,
        resolved_headless_browser_state(&args),
    )?;

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

fn run_export_browser_state(args: ExportBrowserStateArgs) -> Result<()> {
    ensure_playwright_available()?;
    export_browser_state_with_playwright(&args.profile, &args.output, &args.origin, args.headless)
        .map(|_| ())
}

fn run_import_browser_state(args: ImportBrowserStateArgs) -> Result<()> {
    ensure_playwright_available()?;
    let browser_state = read_browser_state(&args.input)?;
    import_browser_state_with_playwright(
        &args.profile,
        &browser_state,
        args.headless,
        args.replace_local_storage,
    )
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

fn preflight_browser_state_requirements(args: &MergeArgs) -> Result<()> {
    if !requires_playwright_auto_export(args) && !requires_playwright_apply(args) {
        return Ok(());
    }
    ensure_playwright_available()
}

fn requires_playwright_auto_export(args: &MergeArgs) -> bool {
    if args.skip_browser_state {
        return false;
    }
    let provided = [
        args.browser_state_a.is_some(),
        args.browser_state_b.is_some(),
        args.browser_state_output.is_some(),
    ];
    if provided.iter().any(|item| *item) {
        return false;
    }
    args.auto_export_browser_state || args.merge_from.is_some()
}

fn requires_playwright_apply(args: &MergeArgs) -> bool {
    args.apply && !args.skip_browser_state
}

fn resolved_headless_browser_state(args: &MergeArgs) -> bool {
    if args.no_headless_browser_state {
        return false;
    }
    let _ = args.headless_browser_state;
    true
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
    profile_a: &Path,
    profile_b: &Path,
    browser_state_tempdir: &mut Option<TempDir>,
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

    let should_auto_export = args.auto_export_browser_state || args.merge_from.is_some();
    if !should_auto_export {
        return Ok((None, None, None));
    }

    let tempdir =
        tempfile::tempdir().with_context(|| "Failed to create browser-state temp directory")?;
    let browser_state_a = tempdir.path().join("browser_state_a.json");
    let browser_state_b = tempdir.path().join("browser_state_b.json");
    let browser_state_output = tempdir.path().join("browser_state_merged.json");
    let headless = resolved_headless_browser_state(args);

    export_browser_state_with_playwright(
        profile_a,
        &browser_state_a,
        "https://claude.ai",
        headless,
    )?;
    export_browser_state_with_playwright(
        profile_b,
        &browser_state_b,
        "https://claude.ai",
        headless,
    )?;

    *browser_state_tempdir = Some(tempdir);
    Ok((
        Some(browser_state_a),
        Some(browser_state_b),
        Some(browser_state_output),
    ))
}

fn apply_merged_profile_if_requested(
    args: &MergeArgs,
    summary: &MergeSummary,
    profile_a: &Path,
    headless_browser_state: bool,
) -> Result<Option<PathBuf>> {
    if !args.apply {
        return Ok(None);
    }

    abort_if_claude_running()?;

    if !args.skip_browser_state {
        let Some(browser_state_output) = summary.browser_state_output.as_ref() else {
            bail!(
                "--apply requires merged browser-state output when browser-state merge is enabled"
            );
        };
        let merged_state = read_browser_state(browser_state_output)?;
        import_browser_state_with_playwright(
            &summary.output_profile,
            &merged_state,
            headless_browser_state,
            true,
        )?;
    }

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

#[cfg(test)]
mod tests {
    use super::{
        requires_playwright_apply, requires_playwright_auto_export,
        resolved_headless_browser_state, MergeArgs,
    };

    #[test]
    fn requires_playwright_auto_export_for_merge_from_defaults() {
        let mut args = base_merge_args();
        args.merge_from = Some("user@host".to_string());

        assert!(requires_playwright_auto_export(&args));
    }

    #[test]
    fn requires_playwright_auto_export_false_when_paths_provided() {
        let mut args = base_merge_args();
        args.merge_from = Some("user@host".to_string());
        args.browser_state_a = Some("/tmp/a.json".into());
        args.browser_state_b = Some("/tmp/b.json".into());
        args.browser_state_output = Some("/tmp/out.json".into());

        assert!(!requires_playwright_auto_export(&args));
    }

    #[test]
    fn requires_playwright_auto_export_false_when_skipped() {
        let mut args = base_merge_args();
        args.merge_from = Some("user@host".to_string());
        args.skip_browser_state = true;

        assert!(!requires_playwright_auto_export(&args));
    }

    #[test]
    fn requires_playwright_apply_only_when_apply_and_not_skipped() {
        let mut args = base_merge_args();
        args.apply = true;
        assert!(requires_playwright_apply(&args));

        args.skip_browser_state = true;
        assert!(!requires_playwright_apply(&args));
    }

    #[test]
    fn resolved_headless_default_true_and_disable_flag_false() {
        let args = base_merge_args();
        assert!(resolved_headless_browser_state(&args));

        let mut no_headless = base_merge_args();
        no_headless.no_headless_browser_state = true;
        assert!(!resolved_headless_browser_state(&no_headless));
    }

    fn base_merge_args() -> MergeArgs {
        MergeArgs {
            profile_a: None,
            profile_b: None,
            merge_from: None,
            remote_profile_path: "Library/Application Support/Claude".to_string(),
            output_profile: None,
            browser_state_a: None,
            browser_state_b: None,
            browser_state_output: None,
            auto_export_browser_state: false,
            headless_browser_state: false,
            no_headless_browser_state: false,
            base_source: "a".to_string(),
            skip_browser_state: false,
            skip_indexeddb: false,
            include_vm_bundles: false,
            include_cache_dirs: false,
            apply: false,
            force: false,
            include_sensitive_claude_credentials: false,
        }
    }
}
