use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use log::warn;
use regex::Regex;
use serde_json::Value;

use crate::models::{BrowserStateExport, CoworkReadState, IndexedDbRecord, SessionBinding};
use crate::utils::parse_int_timestamp;

pub fn read_browser_state(path: &Path) -> Result<BrowserStateExport> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let parsed: BrowserStateExport = serde_json::from_str(&raw)
        .with_context(|| format!("Invalid browser state file: {}", path.display()))?;
    Ok(parsed)
}

pub fn write_browser_state(path: &Path, state: &BrowserStateExport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory for {}", path.display()))?;
    }
    let rendered = serde_json::to_string_pretty(state)
        .with_context(|| format!("Failed to serialize browser state to {}", path.display()))?;
    fs::write(path, format!("{rendered}\n"))
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn merge_browser_states(
    state_a: &BrowserStateExport,
    state_b: &BrowserStateExport,
    session_bindings: &HashMap<String, SessionBinding>,
    base_source: &str,
    profile_a_mtime_ms: i64,
    profile_b_mtime_ms: i64,
    merge_indexeddb: bool,
) -> BrowserStateExport {
    let local_storage = merge_local_storage(
        &state_a.local_storage,
        &state_b.local_storage,
        session_bindings,
        base_source,
        profile_a_mtime_ms,
        profile_b_mtime_ms,
    );

    let indexed_db = if merge_indexeddb {
        merge_indexed_db(&state_a.indexed_db, &state_b.indexed_db, base_source)
    } else {
        HashMap::new()
    };

    BrowserStateExport {
        schema_version: "1".to_string(),
        origin: if base_source == "a" {
            state_a.origin.clone()
        } else {
            state_b.origin.clone()
        },
        exported_at: Utc::now().timestamp_millis(),
        local_storage,
        indexed_db,
    }
}

fn merge_local_storage(
    local_a: &HashMap<String, String>,
    local_b: &HashMap<String, String>,
    session_bindings: &HashMap<String, SessionBinding>,
    base_source: &str,
    profile_a_mtime_ms: i64,
    profile_b_mtime_ms: i64,
) -> HashMap<String, String> {
    let (base, other) = if base_source == "a" {
        (local_a, local_b)
    } else {
        (local_b, local_a)
    };

    let mut merged = base.clone();
    for (key, value) in other {
        merged.entry(key.clone()).or_insert_with(|| value.clone());
    }

    merged.insert(
        "cowork-read-state".to_string(),
        merge_cowork_read_state(local_a, local_b, session_bindings),
    );
    merge_draft_keys(
        &mut merged,
        local_a,
        local_b,
        profile_a_mtime_ms,
        profile_b_mtime_ms,
    );
    hydrate_session_bindings(&mut merged, session_bindings);
    merged
}

fn merge_cowork_read_state(
    local_a: &HashMap<String, String>,
    local_b: &HashMap<String, String>,
    session_bindings: &HashMap<String, SessionBinding>,
) -> String {
    let parsed_a = parse_cowork_read_state(local_a.get("cowork-read-state"));
    let parsed_b = parse_cowork_read_state(local_b.get("cowork-read-state"));

    let mut merged_sessions = parsed_a.sessions;
    for (session_id, timestamp) in parsed_b.sessions {
        let current = merged_sessions.entry(session_id).or_insert(0);
        *current = (*current).max(timestamp);
    }
    for (session_id, binding) in session_bindings {
        let current = merged_sessions.entry(session_id.clone()).or_insert(0);
        *current = (*current).max(binding.last_activity_at);
    }

    let initialized_at = match (parsed_a.initialized_at, parsed_b.initialized_at) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    serde_json::to_string(&CoworkReadState {
        sessions: merged_sessions,
        initialized_at,
    })
    .unwrap_or_else(|_| String::from("{\"sessions\":{}}"))
}

fn parse_cowork_read_state(raw: Option<&String>) -> CoworkReadState {
    let Some(raw_value) = raw else {
        return CoworkReadState {
            sessions: HashMap::new(),
            initialized_at: None,
        };
    };

    match serde_json::from_str::<CoworkReadState>(raw_value) {
        Ok(value) => value,
        Err(_) => {
            warn!("Ignoring malformed cowork-read-state payload");
            CoworkReadState {
                sessions: HashMap::new(),
                initialized_at: None,
            }
        }
    }
}

fn merge_draft_keys(
    merged: &mut HashMap<String, String>,
    local_a: &HashMap<String, String>,
    local_b: &HashMap<String, String>,
    profile_a_mtime_ms: i64,
    profile_b_mtime_ms: i64,
) {
    let pattern = Regex::new(r"^local_[^:]+:(attachment|files|textInput)$").expect("valid regex");
    let candidate_keys: std::collections::BTreeSet<String> = local_a
        .keys()
        .chain(local_b.keys())
        .filter(|key| pattern.is_match(key))
        .cloned()
        .collect();

    for key in candidate_keys {
        let value_a = local_a.get(&key);
        let value_b = local_b.get(&key);

        match (value_a, value_b) {
            (None, Some(b)) => {
                merged.insert(key, b.clone());
            }
            (Some(a), None) => {
                merged.insert(key, a.clone());
            }
            (Some(a), Some(b)) => {
                let winner = pick_newer_payload(a, b, profile_a_mtime_ms, profile_b_mtime_ms);
                merged.insert(key, winner);
            }
            (None, None) => {}
        }
    }
}

fn pick_newer_payload(
    value_a: &str,
    value_b: &str,
    profile_a_mtime_ms: i64,
    profile_b_mtime_ms: i64,
) -> String {
    let ts_a = embedded_timestamp(value_a);
    let ts_b = embedded_timestamp(value_b);

    match (ts_a, ts_b) {
        (Some(a), Some(b)) => {
            if a >= b {
                value_a.to_string()
            } else {
                value_b.to_string()
            }
        }
        (Some(_), None) => value_a.to_string(),
        (None, Some(_)) => value_b.to_string(),
        (None, None) => {
            if profile_a_mtime_ms >= profile_b_mtime_ms {
                value_a.to_string()
            } else {
                value_b.to_string()
            }
        }
    }
}

fn embedded_timestamp(raw: &str) -> Option<i64> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let object = parsed.as_object()?;
    for key in ["updatedAt", "timestamp", "updated_at", "updatedAtMs"] {
        if let Some(timestamp) = object.get(key).and_then(parse_int_timestamp) {
            return Some(timestamp);
        }
    }
    None
}

fn hydrate_session_bindings(
    merged: &mut HashMap<String, String>,
    session_bindings: &HashMap<String, SessionBinding>,
) {
    for (session_id, binding) in session_bindings {
        if let Some(cli_session_id) = &binding.cli_session_id {
            merged.insert(
                format!("cc-session-cli-id-{session_id}"),
                cli_session_id.clone(),
            );
        }
        if let Some(cwd) = &binding.cwd {
            merged.insert(format!("cc-session-cwd-{session_id}"), cwd.clone());
        }
    }
}

fn merge_indexed_db(
    indexed_a: &HashMap<String, Vec<IndexedDbRecord>>,
    indexed_b: &HashMap<String, Vec<IndexedDbRecord>>,
    base_source: &str,
) -> HashMap<String, Vec<IndexedDbRecord>> {
    let (base, other) = if base_source == "a" {
        (indexed_a, indexed_b)
    } else {
        (indexed_b, indexed_a)
    };

    let stores: std::collections::BTreeSet<String> =
        indexed_a.keys().chain(indexed_b.keys()).cloned().collect();

    let mut merged = HashMap::new();
    for store_name in stores {
        merged.insert(
            store_name.clone(),
            merge_store_rows(
                base.get(&store_name).cloned().unwrap_or_default(),
                other.get(&store_name).cloned().unwrap_or_default(),
            ),
        );
    }
    merged
}

fn merge_store_rows(
    base_rows: Vec<IndexedDbRecord>,
    other_rows: Vec<IndexedDbRecord>,
) -> Vec<IndexedDbRecord> {
    let mut rows_by_key: HashMap<String, IndexedDbRecord> = HashMap::new();
    for row in base_rows {
        rows_by_key.insert(serialize_key(&row.key), row);
    }

    for row in other_rows {
        let marker = serialize_key(&row.key);
        match rows_by_key.get(&marker) {
            None => {
                rows_by_key.insert(marker, row);
            }
            Some(current) => {
                if is_other_row_newer(current, &row) {
                    rows_by_key.insert(marker, row);
                }
            }
        }
    }

    let mut keys: Vec<String> = rows_by_key.keys().cloned().collect();
    keys.sort();
    keys.into_iter()
        .filter_map(|key| rows_by_key.remove(&key))
        .collect()
}

fn serialize_key(key: &Value) -> String {
    serde_json::to_string(key).unwrap_or_else(|_| String::from("null"))
}

fn is_other_row_newer(base_row: &IndexedDbRecord, other_row: &IndexedDbRecord) -> bool {
    match (
        timestamp_from_value(&base_row.value),
        timestamp_from_value(&other_row.value),
    ) {
        (Some(base_ts), Some(other_ts)) => other_ts > base_ts,
        _ => false,
    }
}

fn timestamp_from_value(value: &Value) -> Option<i64> {
    let object = value.as_object()?;
    for key in ["updatedAt", "timestamp", "updated_at", "updatedAtMs"] {
        if let Some(parsed) = object.get(key).and_then(parse_int_timestamp) {
            return Some(parsed);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use crate::models::{BrowserStateExport, IndexedDbRecord, SessionBinding};

    use super::merge_browser_states;

    #[test]
    fn merge_browser_states_applies_cowork_rules() {
        let state_a = BrowserStateExport {
            schema_version: "1".to_string(),
            origin: "https://claude.ai".to_string(),
            exported_at: 1,
            local_storage: HashMap::from([
                (
                    "cowork-read-state".to_string(),
                    serde_json::to_string(
                        &json!({"sessions": {"local_shared": 10}, "initializedAt": 40}),
                    )
                    .expect("json string"),
                ),
                (
                    "cc-session-cli-id-local_shared".to_string(),
                    "cli-old".to_string(),
                ),
                (
                    "local_shared:textInput".to_string(),
                    serde_json::to_string(&json!({"updatedAt": 100, "value": "a"}))
                        .expect("json string"),
                ),
                ("unknown-preference".to_string(), "A".to_string()),
            ]),
            indexed_db: HashMap::new(),
        };

        let state_b = BrowserStateExport {
            schema_version: "1".to_string(),
            origin: "https://claude.ai".to_string(),
            exported_at: 2,
            local_storage: HashMap::from([
                (
                    "cowork-read-state".to_string(),
                    serde_json::to_string(&json!({"sessions": {"local_shared": 20, "local_new": 30}, "initializedAt": 50}))
                        .expect("json string"),
                ),
                (
                    "local_shared:textInput".to_string(),
                    serde_json::to_string(&json!({"updatedAt": 200, "value": "b"})).expect("json string"),
                ),
                (
                    "local_new:files".to_string(),
                    serde_json::to_string(&json!({"timestamp": 123, "value": "files"})).expect("json string"),
                ),
                ("other-key".to_string(), "B".to_string()),
            ]),
            indexed_db: HashMap::new(),
        };

        let bindings = HashMap::from([
            (
                "local_shared".to_string(),
                SessionBinding {
                    session_id: "local_shared".to_string(),
                    last_activity_at: 300,
                    cli_session_id: Some("cli-merged".to_string()),
                    cwd: Some("/repo".to_string()),
                },
            ),
            (
                "local_new".to_string(),
                SessionBinding {
                    session_id: "local_new".to_string(),
                    last_activity_at: 400,
                    cli_session_id: Some("cli-new".to_string()),
                    cwd: None,
                },
            ),
        ]);

        let merged = merge_browser_states(&state_a, &state_b, &bindings, "a", 10, 20, false);
        let read_state: serde_json::Value = serde_json::from_str(
            merged
                .local_storage
                .get("cowork-read-state")
                .expect("read-state"),
        )
        .expect("json");

        assert_eq!(read_state["initializedAt"], serde_json::Value::from(40));
        assert_eq!(
            read_state["sessions"]["local_shared"],
            serde_json::Value::from(300)
        );
        assert_eq!(
            read_state["sessions"]["local_new"],
            serde_json::Value::from(400)
        );
        assert_eq!(
            merged.local_storage.get("cc-session-cli-id-local_shared"),
            Some(&"cli-merged".to_string())
        );
        assert_eq!(
            merged.local_storage.get("cc-session-cwd-local_shared"),
            Some(&"/repo".to_string())
        );
        assert_eq!(
            merged.local_storage.get("local_shared:textInput"),
            Some(
                &serde_json::to_string(&json!({"updatedAt": 200, "value": "b"}))
                    .expect("json string")
            )
        );
        assert_eq!(
            merged.local_storage.get("unknown-preference"),
            Some(&"A".to_string())
        );
        assert_eq!(
            merged.local_storage.get("other-key"),
            Some(&"B".to_string())
        );
    }

    #[test]
    fn merge_browser_states_merges_indexeddb_with_timestamp_wins() {
        let state_a = BrowserStateExport {
            schema_version: "1".to_string(),
            origin: "https://claude.ai".to_string(),
            exported_at: 1,
            local_storage: HashMap::new(),
            indexed_db: HashMap::from([(
                "db::store".to_string(),
                vec![
                    IndexedDbRecord {
                        key: json!("k1"),
                        value: json!({"updatedAt": 100, "text": "old"}),
                    },
                    IndexedDbRecord {
                        key: json!("k2"),
                        value: json!({"text": "keep-base-no-timestamp"}),
                    },
                ],
            )]),
        };

        let state_b = BrowserStateExport {
            schema_version: "1".to_string(),
            origin: "https://claude.ai".to_string(),
            exported_at: 2,
            local_storage: HashMap::new(),
            indexed_db: HashMap::from([(
                "db::store".to_string(),
                vec![
                    IndexedDbRecord {
                        key: json!("k1"),
                        value: json!({"updatedAt": 200, "text": "new"}),
                    },
                    IndexedDbRecord {
                        key: json!("k2"),
                        value: json!({"text": "other-no-timestamp"}),
                    },
                    IndexedDbRecord {
                        key: json!("k3"),
                        value: json!({"updatedAt": 1, "text": "insert"}),
                    },
                ],
            )]),
        };

        let merged = merge_browser_states(&state_a, &state_b, &HashMap::new(), "a", 1, 1, true);
        let store = merged.indexed_db.get("db::store").expect("store exists");

        let mut by_key = HashMap::new();
        for row in store {
            by_key.insert(row.key.to_string(), row.value.clone());
        }

        assert_eq!(
            by_key
                .get("\"k1\"")
                .and_then(|value| value.get("text"))
                .and_then(|value| value.as_str()),
            Some("new")
        );
        assert_eq!(
            by_key
                .get("\"k2\"")
                .and_then(|value| value.get("text"))
                .and_then(|value| value.as_str()),
            Some("keep-base-no-timestamp")
        );
        assert_eq!(
            by_key
                .get("\"k3\"")
                .and_then(|value| value.get("text"))
                .and_then(|value| value.as_str()),
            Some("insert")
        );
    }
}
