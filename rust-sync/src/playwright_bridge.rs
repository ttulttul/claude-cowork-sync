use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use log::{info, warn};

pub fn ensure_playwright_available() -> Result<()> {
    info!("Checking Playwright runtime availability via Python CLI preflight");
    let result = run_uv_in_repo_root([
        "run",
        "python",
        "-c",
        "from claude_cowork_sync.cli import _ensure_playwright_available_for_auto_export as check; check()",
    ])?;
    if !result.status.success() {
        bail!(
            "Playwright preflight failed: {}",
            stderr_or_stdout(&result.stderr, &result.stdout)
        );
    }
    Ok(())
}

pub fn export_browser_state_with_playwright(
    profile_dir: &Path,
    output_path: &Path,
    origin: &str,
    headless: bool,
) -> Result<()> {
    info!(
        "Exporting browser state via Playwright bridge for profile {}",
        profile_dir.display()
    );

    let profile = profile_dir
        .to_str()
        .with_context(|| format!("Non-UTF8 profile path: {}", profile_dir.display()))?;
    let output = output_path
        .to_str()
        .with_context(|| format!("Non-UTF8 output path: {}", output_path.display()))?;

    let mut args = vec![
        "run",
        "cowork-merge",
        "export-browser-state",
        "--profile",
        profile,
        "--output",
        output,
        "--origin",
        origin,
    ];
    if headless {
        args.push("--headless");
    }

    let result = run_uv_in_repo_root(args)?;
    if !result.status.success() {
        bail!(
            "Playwright export failed: {}",
            stderr_or_stdout(&result.stderr, &result.stdout)
        );
    }
    Ok(())
}

pub fn import_browser_state_with_playwright(
    profile_dir: &Path,
    input_path: &Path,
    headless: bool,
    replace_local_storage: bool,
) -> Result<()> {
    info!(
        "Importing browser state via Playwright bridge into profile {}",
        profile_dir.display()
    );

    let profile = profile_dir
        .to_str()
        .with_context(|| format!("Non-UTF8 profile path: {}", profile_dir.display()))?;
    let input = input_path
        .to_str()
        .with_context(|| format!("Non-UTF8 input path: {}", input_path.display()))?;

    let mut args = vec![
        "run",
        "cowork-merge",
        "import-browser-state",
        "--profile",
        profile,
        "--input",
        input,
    ];
    if headless {
        args.push("--headless");
    }
    if replace_local_storage {
        args.push("--replace-local-storage");
    }

    let result = run_uv_in_repo_root(args)?;
    if !result.status.success() {
        bail!(
            "Playwright import failed: {}",
            stderr_or_stdout(&result.stderr, &result.stdout)
        );
    }
    Ok(())
}

fn run_uv_in_repo_root<I, S>(args: I) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let repo_root = find_repo_root_with_pyproject()?;
    let output = Command::new("uv")
        .args(args)
        .current_dir(&repo_root)
        .output()
        .with_context(|| {
            format!(
                "Failed to run uv command in repo root {}",
                repo_root.display()
            )
        })?;
    Ok(output)
}

fn find_repo_root_with_pyproject() -> Result<PathBuf> {
    let cwd = env::current_dir().with_context(|| "Failed to read current directory")?;
    for candidate in cwd.ancestors() {
        let pyproject = candidate.join("pyproject.toml");
        if pyproject.exists() {
            return Ok(candidate.to_path_buf());
        }
    }
    warn!("No pyproject.toml found in current directory ancestors; using current directory");
    Ok(cwd)
}

fn stderr_or_stdout(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr_text.is_empty() {
        return stderr_text;
    }
    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    if !stdout_text.is_empty() {
        return stdout_text;
    }
    "command failed without stderr/stdout output".to_string()
}
