use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use log::{info, warn};
use rayon::prelude::*;
use serde_json::{Map, Value};
use walkdir::WalkDir;

use crate::metadata_merge::merge_session_metadata;
use crate::models::{SessionBinding, SessionMergeResult, SessionSourceRecord};
use crate::progress::{ProgressColor, TerminalProgress};
use crate::utils::{
    conflict_path, ensure_parent, parse_int_timestamp, read_json_object, sha256_file, sha256_text,
    write_json_object,
};

#[derive(Debug, Clone)]
struct AuditLine {
    raw_line: String,
    dedupe_key: String,
    source_rank: i32,
    line_index: usize,
    timestamp: Option<i64>,
}

#[derive(Debug, Clone)]
struct SecondaryMergeEntry {
    source_file: PathBuf,
    target_file: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuickCompareResult {
    Equal,
    Different,
    NeedsHashing,
}

pub fn discover_session_records(
    profile_dir: &Path,
    source_label: &str,
) -> Result<BTreeMap<String, SessionSourceRecord>> {
    let sessions_root = profile_dir.join("local-agent-mode-sessions");
    let mut records: BTreeMap<String, SessionSourceRecord> = BTreeMap::new();
    if !sessions_root.exists() {
        warn!(
            "Sessions root missing for source {}: {}",
            source_label,
            sessions_root.display()
        );
        return Ok(records);
    }

    for entry in WalkDir::new(&sessions_root)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !file_name.starts_with("local_") || !file_name.ends_with(".json") {
            continue;
        }

        let Some(record) = build_session_record(profile_dir, source_label, &sessions_root, path)?
        else {
            continue;
        };

        let chosen = choose_preferred_record(records.get(&record.session_id), &record);
        records.insert(record.session_id.clone(), chosen);
    }

    info!(
        "Discovered {} sessions for source {}",
        records.len(),
        source_label
    );
    Ok(records)
}

pub fn merge_session_trees(
    profile_a: &Path,
    profile_b: &Path,
    output_profile: &Path,
    include_sensitive_claude_credentials: bool,
    parallel_local: usize,
) -> Result<BTreeMap<String, SessionMergeResult>> {
    let records_a = discover_session_records(profile_a, "a")?;
    let records_b = discover_session_records(profile_b, "b")?;

    let session_ids: Vec<String> = records_a
        .keys()
        .chain(records_b.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let progress = Arc::new(Mutex::new(TerminalProgress::new(
        "Merging sessions",
        if session_ids.is_empty() {
            None
        } else {
            Some(session_ids.len() as u64)
        },
        "sessions",
        ProgressColor::Green,
    )));
    let merged_count = Arc::new(AtomicU64::new(0));

    let thread_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_local.max(1))
        .build()
        .with_context(|| "Failed to build local thread pool for session merge")?;

    let merge_entries: Vec<Result<(String, SessionMergeResult)>> = thread_pool.install(|| {
        session_ids
            .par_iter()
            .map(|session_id| {
                let record_a = records_a.get(session_id);
                let record_b = records_b.get(session_id);
                let result = merge_single_session(
                    record_a,
                    record_b,
                    output_profile,
                    include_sensitive_claude_credentials,
                )?;

                let completed = merged_count.fetch_add(1, Ordering::Relaxed) + 1;
                if let Ok(mut progress_guard) = progress.lock() {
                    progress_guard.update(completed, &format!("merged={completed}"), false);
                }

                Ok((session_id.clone(), result))
            })
            .collect()
    });

    let mut merged_results = BTreeMap::new();
    for merge_entry in merge_entries {
        let (session_id, merge_result) = merge_entry?;
        merged_results.insert(session_id, merge_result);
    }

    if let Ok(mut progress_guard) = progress.lock() {
        progress_guard.finish(
            merged_results.len() as u64,
            &format!("merged={}", merged_results.len()),
            true,
        );
    }

    info!(
        "Merged {} sessions into {}",
        merged_results.len(),
        output_profile.display()
    );
    Ok(merged_results)
}

fn merge_single_session(
    record_a: Option<&SessionSourceRecord>,
    record_b: Option<&SessionSourceRecord>,
    output_profile: &Path,
    include_sensitive_claude_credentials: bool,
) -> Result<SessionMergeResult> {
    match (record_a, record_b) {
        (Some(a), Some(b)) => {
            merge_shared_session(a, b, output_profile, include_sensitive_claude_credentials)
        }
        (Some(a), None) => Ok(build_existing_result(output_profile, a)),
        (None, Some(b)) => {
            copy_session_from_secondary(b, output_profile, include_sensitive_claude_credentials)
        }
        (None, None) => bail!("Session merge expected at least one source record"),
    }
}

fn build_session_record(
    profile_dir: &Path,
    source_label: &str,
    sessions_root: &Path,
    json_path: &Path,
) -> Result<Option<SessionSourceRecord>> {
    let session_id = json_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();

    if !session_id.starts_with("local_") {
        return Ok(None);
    }

    let metadata = read_json_object(json_path)?;
    let parent = json_path
        .parent()
        .with_context(|| format!("JSON session path has no parent: {}", json_path.display()))?;
    let relative_group_dir = parent
        .strip_prefix(sessions_root)
        .with_context(|| format!("Failed to compute group dir for {}", json_path.display()))?
        .to_path_buf();
    let folder_path = parent.join(&session_id);

    Ok(Some(SessionSourceRecord {
        source_label: source_label.to_string(),
        session_id,
        profile_dir: profile_dir.to_path_buf(),
        json_path: json_path.to_path_buf(),
        folder_path,
        relative_group_dir,
        metadata,
    }))
}

fn choose_preferred_record(
    existing: Option<&SessionSourceRecord>,
    candidate: &SessionSourceRecord,
) -> SessionSourceRecord {
    let Some(existing_record) = existing else {
        return candidate.clone();
    };

    let existing_last = existing_record
        .metadata
        .get("lastActivityAt")
        .and_then(parse_int_timestamp)
        .unwrap_or(0);
    let candidate_last = candidate
        .metadata
        .get("lastActivityAt")
        .and_then(parse_int_timestamp)
        .unwrap_or(0);

    if candidate_last >= existing_last {
        warn!(
            "Found duplicate session {} in {}. Using newer {}",
            candidate.session_id,
            candidate.source_label,
            candidate.json_path.display()
        );
        candidate.clone()
    } else {
        existing_record.clone()
    }
}

fn merge_shared_session(
    record_a: &SessionSourceRecord,
    record_b: &SessionSourceRecord,
    output_profile: &Path,
    include_sensitive_claude_credentials: bool,
) -> Result<SessionMergeResult> {
    let (output_json_path, output_folder_path) = output_paths_for_record(output_profile, record_a)?;
    let merged_metadata = merge_session_metadata(&record_a.metadata, &record_b.metadata);
    write_json_object(&output_json_path, &merged_metadata)?;

    merge_audit_file(
        &record_a.folder_path,
        &record_b.folder_path,
        &output_folder_path,
    )?;
    merge_secondary_folder_files(
        &record_b.folder_path,
        &output_folder_path,
        &record_b.source_label,
        include_sensitive_claude_credentials,
    )?;

    let binding = build_binding(&record_a.session_id, &merged_metadata);
    Ok(SessionMergeResult {
        session_id: record_a.session_id.clone(),
        json_path: output_json_path,
        folder_path: output_folder_path,
        binding,
    })
}

fn copy_session_from_secondary(
    record_b: &SessionSourceRecord,
    output_profile: &Path,
    include_sensitive_claude_credentials: bool,
) -> Result<SessionMergeResult> {
    let (output_json_path, output_folder_path) = output_paths_for_record(output_profile, record_b)?;
    ensure_parent(&output_json_path)?;
    fs::copy(&record_b.json_path, &output_json_path).with_context(|| {
        format!(
            "Failed to copy secondary session JSON {} to {}",
            record_b.json_path.display(),
            output_json_path.display()
        )
    })?;

    if record_b.folder_path.exists() {
        fs::create_dir_all(&output_folder_path)
            .with_context(|| format!("Failed to create {}", output_folder_path.display()))?;
        merge_secondary_folder_files(
            &record_b.folder_path,
            &output_folder_path,
            &record_b.source_label,
            include_sensitive_claude_credentials,
        )?;
    }

    let binding = build_binding(&record_b.session_id, &record_b.metadata);
    Ok(SessionMergeResult {
        session_id: record_b.session_id.clone(),
        json_path: output_json_path,
        folder_path: output_folder_path,
        binding,
    })
}

fn build_existing_result(
    output_profile: &Path,
    record: &SessionSourceRecord,
) -> SessionMergeResult {
    let (output_json_path, output_folder_path) = output_paths_for_record(output_profile, record)
        .unwrap_or_else(|_| {
            (
                output_profile
                    .join("local-agent-mode-sessions")
                    .join(&record.relative_group_dir)
                    .join(format!("{}.json", record.session_id)),
                output_profile
                    .join("local-agent-mode-sessions")
                    .join(&record.relative_group_dir)
                    .join(&record.session_id),
            )
        });

    let binding = build_binding(&record.session_id, &record.metadata);
    SessionMergeResult {
        session_id: record.session_id.clone(),
        json_path: output_json_path,
        folder_path: output_folder_path,
        binding,
    }
}

fn output_paths_for_record(
    output_profile: &Path,
    record: &SessionSourceRecord,
) -> Result<(PathBuf, PathBuf)> {
    let parent = output_profile
        .join("local-agent-mode-sessions")
        .join(&record.relative_group_dir);
    Ok((
        parent.join(format!("{}.json", record.session_id)),
        parent.join(&record.session_id),
    ))
}

fn build_binding(session_id: &str, metadata: &Map<String, Value>) -> SessionBinding {
    let cli_session_id = extract_first_string(
        metadata,
        &["cliSessionId", "cli_session_id", "sessionCliId", "cliId"],
    );
    let cwd = extract_first_string(metadata, &["cwd", "workingDirectory", "sessionCwd"]);
    let last_activity_at = metadata
        .get("lastActivityAt")
        .and_then(parse_int_timestamp)
        .unwrap_or(0);

    SessionBinding {
        session_id: session_id.to_string(),
        last_activity_at,
        cli_session_id,
        cwd,
    }
}

fn extract_first_string(data: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        let value = data
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn merge_audit_file(folder_a: &Path, folder_b: &Path, output_folder: &Path) -> Result<()> {
    let mut lines = read_audit_lines(&folder_a.join("audit.jsonl"), 0)?;
    lines.extend(read_audit_lines(&folder_b.join("audit.jsonl"), 1)?);
    let merged = dedupe_and_sort_audit_lines(lines);

    fs::create_dir_all(output_folder)
        .with_context(|| format!("Failed to create {}", output_folder.display()))?;
    let output_path = output_folder.join("audit.jsonl");
    let mut rendered = String::new();
    for line in merged {
        rendered.push_str(&line.raw_line);
        if !line.raw_line.ends_with('\n') {
            rendered.push('\n');
        }
    }
    fs::write(&output_path, rendered)
        .with_context(|| format!("Failed to write {}", output_path.display()))?;
    Ok(())
}

fn read_audit_lines(path: &Path, source_rank: i32) -> Result<Vec<AuditLine>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut lines = Vec::new();
    for (index, raw_line) in raw.lines().enumerate() {
        let (dedupe_key, timestamp) = dedupe_key_and_timestamp(raw_line);
        lines.push(AuditLine {
            raw_line: raw_line.to_string(),
            dedupe_key,
            source_rank,
            line_index: index,
            timestamp,
        });
    }
    Ok(lines)
}

fn dedupe_key_and_timestamp(raw_line: &str) -> (String, Option<i64>) {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() {
        let digest = sha256_text(raw_line);
        return (format!("raw:{digest}"), None);
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        let digest = sha256_text(trimmed);
        return (format!("raw:{digest}"), None);
    };

    let Some(object) = value.as_object() else {
        let digest = sha256_text(trimmed);
        return (format!("raw:{digest}"), None);
    };

    let entry_uuid = extract_first_string(object, &["uuid", "_audit_uuid", "eventId", "id"]);
    let timestamp = object.get("_audit_timestamp").and_then(parse_int_timestamp);

    if let Some(uuid) = entry_uuid {
        (format!("uuid:{uuid}"), timestamp)
    } else {
        let digest = sha256_text(trimmed);
        (format!("raw:{digest}"), timestamp)
    }
}

fn dedupe_and_sort_audit_lines(lines: Vec<AuditLine>) -> Vec<AuditLine> {
    let mut deduped: HashMap<String, AuditLine> = HashMap::new();
    for line in lines {
        if let Some(current) = deduped.get(&line.dedupe_key) {
            deduped.insert(
                line.dedupe_key.clone(),
                prefer_audit_line(current.clone(), line),
            );
        } else {
            deduped.insert(line.dedupe_key.clone(), line);
        }
    }

    let mut merged: Vec<AuditLine> = deduped.into_values().collect();
    merged.sort_by_key(audit_sort_key);
    merged
}

fn prefer_audit_line(current: AuditLine, candidate: AuditLine) -> AuditLine {
    match (current.timestamp, candidate.timestamp) {
        (None, Some(_)) => return candidate,
        (Some(_), None) => return current,
        (Some(a), Some(b)) => {
            if b > a {
                return candidate;
            }
        }
        _ => {}
    }

    if candidate.source_rank > current.source_rank {
        candidate
    } else {
        current
    }
}

fn audit_sort_key(line: &AuditLine) -> (i32, i64, i64, i64) {
    if let Some(timestamp) = line.timestamp {
        return (
            0,
            timestamp,
            line.source_rank as i64,
            line.line_index as i64,
        );
    }
    (1, line.source_rank as i64, line.line_index as i64, 0)
}

fn merge_secondary_folder_files(
    source_folder: &Path,
    target_folder: &Path,
    source_label: &str,
    include_sensitive_claude_credentials: bool,
) -> Result<()> {
    if !source_folder.exists() {
        return Ok(());
    }

    let mut merge_entries = Vec::new();
    for entry in WalkDir::new(source_folder)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let source_file = entry.path();
        let rel_path = source_file.strip_prefix(source_folder).with_context(|| {
            format!(
                "Failed to compute relative path for {}",
                source_file.display()
            )
        })?;
        let rel_posix = rel_path.to_string_lossy().replace('\\', "/");
        if rel_posix == "audit.jsonl" {
            continue;
        }
        if !include_sensitive_claude_credentials && rel_posix == ".claude/.credentials.json" {
            info!(
                "Skipping sensitive credentials file: {}",
                source_file.display()
            );
            continue;
        }

        merge_entries.push(SecondaryMergeEntry {
            source_file: source_file.to_path_buf(),
            target_file: target_folder.join(rel_path),
        });
    }

    if merge_entries.len() <= 1 {
        for merge_entry in &merge_entries {
            merge_file_into_target(
                &merge_entry.source_file,
                &merge_entry.target_file,
                source_label,
            )?;
        }
        return Ok(());
    }

    let outcomes: Vec<Result<()>> = merge_entries
        .par_iter()
        .map(|merge_entry| {
            merge_file_into_target(
                &merge_entry.source_file,
                &merge_entry.target_file,
                source_label,
            )
        })
        .collect();
    for outcome in outcomes {
        outcome?;
    }

    Ok(())
}

fn merge_file_into_target(
    source_file: &Path,
    target_file: &Path,
    source_label: &str,
) -> Result<()> {
    ensure_parent(target_file)?;
    if !target_file.exists() {
        fs::copy(source_file, target_file).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                source_file.display(),
                target_file.display()
            )
        })?;
        return Ok(());
    }

    let source_hash = match quick_compare_existing_target(source_file, target_file)? {
        QuickCompareResult::Equal => return Ok(()),
        QuickCompareResult::Different => sha256_file(source_file)?,
        QuickCompareResult::NeedsHashing => {
            let source_hash = sha256_file(source_file)?;
            let target_hash = sha256_file(target_file)?;
            if source_hash == target_hash {
                return Ok(());
            }
            source_hash
        }
    };

    let conflict_target = conflict_path(target_file, source_label, &source_hash);
    ensure_parent(&conflict_target)?;

    if !conflict_target.exists() {
        fs::copy(source_file, &conflict_target).with_context(|| {
            format!(
                "Failed to copy conflict {} to {}",
                source_file.display(),
                conflict_target.display()
            )
        })?;
        return Ok(());
    }

    if quick_compare_existing_target(source_file, &conflict_target)? == QuickCompareResult::Equal {
        return Ok(());
    }

    let existing_conflict_hash = sha256_file(&conflict_target)?;
    if existing_conflict_hash != source_hash {
        bail!(
            "Non-deterministic conflict on {}",
            conflict_target.display()
        );
    }

    Ok(())
}

fn quick_compare_existing_target(
    source_file: &Path,
    target_file: &Path,
) -> Result<QuickCompareResult> {
    let source_metadata = fs::metadata(source_file)
        .with_context(|| format!("Failed to stat source file {}", source_file.display()))?;
    let target_metadata = fs::metadata(target_file)
        .with_context(|| format!("Failed to stat target file {}", target_file.display()))?;

    if source_metadata.len() != target_metadata.len() {
        return Ok(QuickCompareResult::Different);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if source_metadata.dev() == target_metadata.dev()
            && source_metadata.ino() == target_metadata.ino()
        {
            return Ok(QuickCompareResult::Equal);
        }
    }

    let source_modified = source_metadata.modified().ok();
    let target_modified = target_metadata.modified().ok();
    if source_modified.is_some() && source_modified == target_modified {
        return Ok(QuickCompareResult::Equal);
    }

    Ok(QuickCompareResult::NeedsHashing)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use serde_json::{json, Value};
    use tempfile::tempdir;
    use walkdir::WalkDir;

    use super::{merge_session_trees, quick_compare_existing_target, QuickCompareResult};

    #[test]
    fn merge_session_trees_merges_metadata_audit_and_payloads() -> Result<()> {
        let tmp = tempdir()?;
        let profile_a = tmp.path().join("profile_a");
        let profile_b = tmp.path().join("profile_b");
        let output = tmp.path().join("merged");

        write_session(
            &profile_a,
            "local_shared",
            json!({
                "createdAt": 100,
                "lastActivityAt": 200,
                "title": "Old title",
                "cliSessionId": "cli-a",
                "cwd": "/a",
                "userApprovedFileAccessPaths": ["/tmp/a"],
                "fsDetectedFiles": [{"hostPath": "/tmp/a.txt", "fileName": "a.txt", "timestamp": 50}],
                "mcqAnswers": {"q1": {"choice": "A"}},
                "enabledMcpTools": {"toolA": true}
            }),
            vec![
                r#"{"uuid":"u1","_audit_timestamp":1000,"message":"first"}"#,
                r#"{"uuid":"u2","_audit_timestamp":2000,"message":"second"}"#,
            ],
            vec![
                ("uploads/note.txt", b"from-a".to_vec()),
                ("outputs/out.txt", b"out-a".to_vec()),
            ],
            b"secret-a",
        )?;

        write_session(
            &profile_b,
            "local_shared",
            json!({
                "createdAt": 150,
                "lastActivityAt": 300,
                "title": "New title",
                "cliSessionId": "cli-b",
                "cwd": "/b",
                "userApprovedFileAccessPaths": ["/tmp/b"],
                "fsDetectedFiles": [{"hostPath": "/tmp/a.txt", "fileName": "a2.txt", "timestamp": 75}],
                "mcqAnswers": {"q1": {"choice": "B"}, "q2": {"choice": "C"}},
                "enabledMcpTools": {"toolB": true}
            }),
            vec![
                r#"{"uuid":"u2","_audit_timestamp":2000,"message":"second-duplicate"}"#,
                r#"{"uuid":"u3","_audit_timestamp":3000,"message":"third"}"#,
            ],
            vec![
                ("uploads/note.txt", b"from-b".to_vec()),
                ("outputs/out2.txt", b"out-b".to_vec()),
            ],
            b"secret-b",
        )?;

        write_session(
            &profile_b,
            "local_only_b",
            json!({"createdAt": 10, "lastActivityAt": 20, "cliSessionId": "cli-c"}),
            vec![r#"{"uuid":"u4","_audit_timestamp":10,"message":"only-b"}"#],
            vec![("uploads/extra.txt", b"extra".to_vec())],
            b"secret-c",
        )?;

        copy_dir(&profile_a, &output)?;

        let merged = merge_session_trees(&profile_a, &profile_b, &output, false, 2)?;

        let shared_json = output.join("local-agent-mode-sessions/user/org/local_shared.json");
        let shared_payload: Value = serde_json::from_str(&fs::read_to_string(shared_json)?)?;
        assert_eq!(shared_payload["createdAt"], Value::from(100));
        assert_eq!(shared_payload["lastActivityAt"], Value::from(300));
        assert_eq!(shared_payload["title"], Value::from("New title"));

        let audit_path = output.join("local-agent-mode-sessions/user/org/local_shared/audit.jsonl");
        let lines: Vec<String> = fs::read_to_string(audit_path)?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(ToString::to_string)
            .collect();
        let uuids: Vec<String> = lines
            .iter()
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .expect("valid audit json")
                    .get("uuid")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(uuids, vec!["u1", "u2", "u3"]);

        let uploads_dir = output.join("local-agent-mode-sessions/user/org/local_shared/uploads");
        let mut upload_files: Vec<String> = fs::read_dir(uploads_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        upload_files.sort();
        assert!(upload_files.iter().any(|name| name == "note.txt"));
        assert!(upload_files.iter().any(|name| name.starts_with("note__b_")));

        let credentials_dir =
            output.join("local-agent-mode-sessions/user/org/local_shared/.claude");
        let mut credential_names: Vec<String> = fs::read_dir(credentials_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        credential_names.sort();
        assert_eq!(credential_names, vec![".credentials.json"]);
        assert!(output
            .join("local-agent-mode-sessions/user/org/local_only_b.json")
            .exists());
        assert_eq!(
            merged
                .get("local_shared")
                .and_then(|result| result.binding.cli_session_id.clone())
                .as_deref(),
            Some("cli-b")
        );

        Ok(())
    }

    #[test]
    fn quick_compare_existing_target_detects_different_size() -> Result<()> {
        let tmp = tempdir()?;
        let source = tmp.path().join("source.txt");
        let target = tmp.path().join("target.txt");
        fs::write(&source, b"abc")?;
        fs::write(&target, b"abcd")?;

        let compare = quick_compare_existing_target(&source, &target)?;
        assert_eq!(compare, QuickCompareResult::Different);
        Ok(())
    }

    #[test]
    fn quick_compare_existing_target_detects_hardlink_equality() -> Result<()> {
        let tmp = tempdir()?;
        let source = tmp.path().join("source.txt");
        let target = tmp.path().join("target.txt");
        fs::write(&source, b"same-data")?;
        fs::hard_link(&source, &target)?;

        let compare = quick_compare_existing_target(&source, &target)?;
        assert_eq!(compare, QuickCompareResult::Equal);
        Ok(())
    }

    fn write_session(
        profile: &std::path::Path,
        session_id: &str,
        metadata: Value,
        audit_lines: Vec<&str>,
        files: Vec<(&str, Vec<u8>)>,
        credentials: &[u8],
    ) -> Result<()> {
        let group = profile.join("local-agent-mode-sessions/user/org");
        fs::create_dir_all(&group)?;
        fs::write(
            group.join(format!("{session_id}.json")),
            serde_json::to_string(&metadata)?,
        )?;

        let folder = group.join(session_id);
        fs::create_dir_all(&folder)?;
        fs::write(
            folder.join("audit.jsonl"),
            format!("{}\n", audit_lines.join("\n")),
        )?;

        for (relative, content) in files {
            let path = folder.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, content)?;
        }

        let credentials_path = folder.join(".claude/.credentials.json");
        if let Some(parent) = credentials_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(credentials_path, credentials)?;
        Ok(())
    }

    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
        for entry in WalkDir::new(src).into_iter().filter_map(|entry| entry.ok()) {
            let rel = entry.path().strip_prefix(src)?;
            let target = dst.join(rel);
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target)?;
            } else if entry.file_type().is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }
}
