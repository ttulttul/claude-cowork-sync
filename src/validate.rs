use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use log::warn;
use serde_json::Value;
use walkdir::WalkDir;

use crate::models::{SessionMergeResult, ValidationResult};

pub fn validate_merged_profile(
    merged_profile: &Path,
    merged_sessions: &BTreeMap<String, SessionMergeResult>,
    local_storage: &HashMap<String, String>,
    enforce_browser_state: bool,
) -> ValidationResult {
    let missing_session_folders = find_missing_session_folders(merged_profile);
    let (missing_cli_binding_keys, missing_cowork_read_state_sessions) = if enforce_browser_state {
        (
            find_missing_cli_bindings(merged_sessions, local_storage),
            find_missing_cowork_read_sessions(merged_sessions, local_storage),
        )
    } else {
        (Vec::new(), Vec::new())
    };

    ValidationResult {
        missing_session_folders,
        missing_cli_binding_keys,
        missing_cowork_read_state_sessions,
    }
}

fn find_missing_session_folders(merged_profile: &Path) -> Vec<String> {
    let sessions_root = merged_profile.join("local-agent-mode-sessions");
    if !sessions_root.exists() {
        return Vec::new();
    }

    let mut missing = Vec::new();
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
        let session_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let Some(parent) = path.parent() else {
            continue;
        };
        let folder_path = parent.join(&session_id);
        if !folder_path.exists() {
            missing.push(session_id);
        }
    }

    missing.sort();
    missing
}

fn find_missing_cli_bindings(
    merged_sessions: &BTreeMap<String, SessionMergeResult>,
    local_storage: &HashMap<String, String>,
) -> Vec<String> {
    let mut missing = Vec::new();
    for (session_id, result) in merged_sessions {
        if result.binding.cli_session_id.is_none() {
            continue;
        }
        let binding_key = format!("cc-session-cli-id-{session_id}");
        if !local_storage.contains_key(&binding_key) {
            missing.push(session_id.clone());
        }
    }
    missing
}

fn find_missing_cowork_read_sessions(
    merged_sessions: &BTreeMap<String, SessionMergeResult>,
    local_storage: &HashMap<String, String>,
) -> Vec<String> {
    let Some(raw) = local_storage.get("cowork-read-state") else {
        return merged_sessions.keys().cloned().collect();
    };

    let parsed = serde_json::from_str::<Value>(raw);
    let Ok(parsed_value) = parsed else {
        warn!("Malformed cowork-read-state during validation");
        return merged_sessions.keys().cloned().collect();
    };

    let sessions = parsed_value
        .as_object()
        .and_then(|object| object.get("sessions"))
        .and_then(Value::as_object);

    let Some(session_object) = sessions else {
        return merged_sessions.keys().cloned().collect();
    };

    merged_sessions
        .keys()
        .filter(|session_id| !session_object.contains_key(*session_id))
        .cloned()
        .collect()
}
