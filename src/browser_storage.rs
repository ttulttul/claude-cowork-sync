use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use log::{info, warn};
use playwright_rs::protocol::{BrowserContextOptions, Playwright};
use playwright_rs::PLAYWRIGHT_VERSION;
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

pub fn ensure_playwright_available() -> Result<()> {
    info!("Checking Playwright runtime availability");
    run_in_tokio_runtime(async {
        let playwright = Playwright::launch()
            .await
            .with_context(|| "Failed to initialize Playwright runtime")?;
        let browser = playwright.chromium().launch().await.with_context(|| {
            format!(
                "Failed to launch Playwright Chromium runtime. Install with `npx playwright@{} install chromium`.",
                PLAYWRIGHT_VERSION
            )
        })?;
        browser
            .close()
            .await
            .with_context(|| "Failed to close Playwright availability-check browser")?;
        Ok(())
    })
}

pub fn export_browser_state_with_playwright(
    profile_dir: &Path,
    output_path: &Path,
    origin: &str,
    headless: bool,
) -> Result<BrowserStateExport> {
    info!(
        "Exporting browser state with native Rust Playwright for {}",
        profile_dir.display()
    );
    let profile = profile_dir.to_path_buf();
    let origin_owned = origin.to_string();
    let (local_storage, indexed_db) = run_in_tokio_runtime(async move {
        run_playwright_export(&profile, &origin_owned, headless).await
    })?;

    let state = BrowserStateExport {
        schema_version: "1".to_string(),
        origin: origin.to_string(),
        exported_at: Utc::now().timestamp_millis(),
        local_storage,
        indexed_db,
    };
    write_browser_state(output_path, &state)?;
    Ok(state)
}

pub fn import_browser_state_with_playwright(
    profile_dir: &Path,
    browser_state: &BrowserStateExport,
    headless: bool,
    replace_local_storage: bool,
) -> Result<()> {
    info!(
        "Importing browser state with native Rust Playwright into {}",
        profile_dir.display()
    );
    let profile = profile_dir.to_path_buf();
    let state = browser_state.clone();
    run_in_tokio_runtime(async move {
        run_playwright_import(&profile, &state, headless, replace_local_storage).await
    })
}

async fn run_playwright_export(
    profile_dir: &Path,
    origin: &str,
    headless: bool,
) -> Result<(
    HashMap<String, String>,
    HashMap<String, Vec<IndexedDbRecord>>,
)> {
    let user_data_dir = profile_dir
        .to_str()
        .with_context(|| format!("Non-UTF8 profile path: {}", profile_dir.display()))?;

    let playwright = Playwright::launch()
        .await
        .with_context(|| "Failed to initialize Playwright runtime")?;
    let options = BrowserContextOptions::builder().headless(headless).build();
    let context = playwright
        .chromium()
        .launch_persistent_context_with_options(user_data_dir, options)
        .await
        .with_context(|| {
            format!(
                "Failed to launch persistent Chromium context. Install browsers with `npx playwright@{} install chromium`.",
                PLAYWRIGHT_VERSION
            )
        })?;

    let page = context
        .new_page()
        .await
        .with_context(|| "Failed to create Playwright page for export")?;
    page.goto(origin, None)
        .await
        .with_context(|| format!("Failed to navigate to {} for browser-state export", origin))?;

    let local_storage: HashMap<String, String> = page
        .evaluate(LOCAL_STORAGE_EXPORT_SCRIPT, None::<&()>)
        .await
        .with_context(|| "Playwright localStorage export returned invalid payload")?;
    let indexed_db_raw: Value = page
        .evaluate(INDEXEDDB_EXPORT_SCRIPT, None::<&()>)
        .await
        .with_context(|| "Playwright IndexedDB export returned invalid payload")?;

    context
        .close()
        .await
        .with_context(|| "Failed to close Playwright export context")?;

    let indexed_db = validate_indexeddb_export(indexed_db_raw);
    Ok((local_storage, indexed_db))
}

async fn run_playwright_import(
    profile_dir: &Path,
    browser_state: &BrowserStateExport,
    headless: bool,
    replace_local_storage: bool,
) -> Result<()> {
    let user_data_dir = profile_dir
        .to_str()
        .with_context(|| format!("Non-UTF8 profile path: {}", profile_dir.display()))?;

    let playwright = Playwright::launch()
        .await
        .with_context(|| "Failed to initialize Playwright runtime")?;
    let options = BrowserContextOptions::builder().headless(headless).build();
    let context = playwright
        .chromium()
        .launch_persistent_context_with_options(user_data_dir, options)
        .await
        .with_context(|| {
            format!(
                "Failed to launch persistent Chromium context for import. Install browsers with `npx playwright@{} install chromium`.",
                PLAYWRIGHT_VERSION
            )
        })?;

    let page = context
        .new_page()
        .await
        .with_context(|| "Failed to create Playwright page for import")?;
    page.goto(&browser_state.origin, None)
        .await
        .with_context(|| {
            format!(
                "Failed to navigate to {} for browser-state import",
                browser_state.origin
            )
        })?;

    let local_storage_payload = serde_json::json!({
        "values": browser_state.local_storage,
        "replace": replace_local_storage,
    });
    let indexed_db_payload = indexeddb_dump(&browser_state.indexed_db);

    let _: Value = page
        .evaluate(LOCAL_STORAGE_IMPORT_SCRIPT, Some(&local_storage_payload))
        .await
        .with_context(|| "Failed to import localStorage via Playwright")?;
    let _: Value = page
        .evaluate(INDEXEDDB_IMPORT_SCRIPT, Some(&indexed_db_payload))
        .await
        .with_context(|| "Failed to import IndexedDB via Playwright")?;

    context
        .close()
        .await
        .with_context(|| "Failed to close Playwright import context")?;

    Ok(())
}

fn validate_indexeddb_export(raw: Value) -> HashMap<String, Vec<IndexedDbRecord>> {
    let Some(stores) = raw.as_object() else {
        return HashMap::new();
    };

    let mut validated = HashMap::new();
    for (store_name, rows) in stores {
        let Some(row_values) = rows.as_array() else {
            continue;
        };
        let mut records = Vec::new();
        for row in row_values {
            match serde_json::from_value::<IndexedDbRecord>(row.clone()) {
                Ok(record) => records.push(record),
                Err(error) => warn!(
                    "Skipping malformed IndexedDB row in store {}: {}",
                    store_name, error
                ),
            }
        }
        validated.insert(store_name.to_string(), records);
    }

    validated
}

fn indexeddb_dump(
    indexed_db: &HashMap<String, Vec<IndexedDbRecord>>,
) -> HashMap<String, Vec<Value>> {
    let mut dumped = HashMap::new();
    for (store, rows) in indexed_db {
        let values = rows
            .iter()
            .filter_map(|row| serde_json::to_value(row).ok())
            .collect::<Vec<_>>();
        dumped.insert(store.clone(), values);
    }
    dumped
}

fn run_in_tokio_runtime<T, F>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .with_context(|| "Failed to initialize tokio runtime for Playwright operation")?;
    runtime.block_on(future)
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

const LOCAL_STORAGE_EXPORT_SCRIPT: &str = r#"
() => {
  const data = {};
  for (let i = 0; i < window.localStorage.length; i += 1) {
    const key = window.localStorage.key(i);
    data[key] = window.localStorage.getItem(key);
  }
  return data;
}
"#;

const LOCAL_STORAGE_IMPORT_SCRIPT: &str = r#"
({ values, replace }) => {
  if (replace) {
    window.localStorage.clear();
  }
  Object.entries(values).forEach(([key, value]) => {
    window.localStorage.setItem(key, value);
  });
}
"#;

const INDEXEDDB_EXPORT_SCRIPT: &str = r#"
async () => {
  const output = {};
  if (!indexedDB.databases) {
    return output;
  }
  const dbs = await indexedDB.databases();
  for (const dbInfo of dbs) {
    if (!dbInfo.name) continue;
    const dbName = dbInfo.name;
    const db = await new Promise((resolve, reject) => {
      const req = indexedDB.open(dbName);
      req.onerror = () => reject(req.error);
      req.onsuccess = () => resolve(req.result);
    });
    const stores = Array.from(db.objectStoreNames);
    for (const storeName of stores) {
      const key = `${dbName}::${storeName}`;
      output[key] = await new Promise((resolve, reject) => {
        const tx = db.transaction(storeName, 'readonly');
        const store = tx.objectStore(storeName);
        const rows = [];
        const req = store.openCursor();
        req.onerror = () => reject(req.error);
        req.onsuccess = () => {
          const cursor = req.result;
          if (!cursor) {
            resolve(rows);
            return;
          }
          rows.push({ key: cursor.key, value: cursor.value });
          cursor.continue();
        };
      });
    }
    db.close();
  }
  return output;
}
"#;

const INDEXEDDB_IMPORT_SCRIPT: &str = r#"
async (stores) => {
  const grouped = {};
  Object.keys(stores).forEach((key) => {
    const parts = key.split('::');
    if (parts.length !== 2) return;
    const [dbName, storeName] = parts;
    grouped[dbName] = grouped[dbName] || {};
    grouped[dbName][storeName] = stores[key];
  });

  const openDb = (dbName, storeNames) => new Promise((resolve, reject) => {
    const req = indexedDB.open(dbName);
    req.onupgradeneeded = () => {
      const db = req.result;
      storeNames.forEach((storeName) => {
        if (!db.objectStoreNames.contains(storeName)) {
          db.createObjectStore(storeName);
        }
      });
    };
    req.onerror = () => reject(req.error);
    req.onsuccess = () => resolve(req.result);
  });

  for (const [dbName, storesForDb] of Object.entries(grouped)) {
    const db = await openDb(dbName, Object.keys(storesForDb));
    for (const [storeName, rows] of Object.entries(storesForDb)) {
      await new Promise((resolve, reject) => {
        const tx = db.transaction(storeName, 'readwrite');
        const store = tx.objectStore(storeName);
        rows.forEach((row) => store.put(row.value, row.key));
        tx.onerror = () => reject(tx.error);
        tx.oncomplete = () => resolve();
      });
    }
    db.close();
  }
}
"#;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use crate::models::{BrowserStateExport, IndexedDbRecord, SessionBinding};

    use super::{merge_browser_states, validate_indexeddb_export};

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
                    serde_json::to_string(&json!({"updatedAt": 200, "value": "b"}))
                        .expect("json string"),
                ),
                (
                    "local_new:files".to_string(),
                    serde_json::to_string(&json!({"timestamp": 123, "value": "files"}))
                        .expect("json string"),
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

    #[test]
    fn validate_indexeddb_export_skips_malformed_rows() {
        let raw = json!({
            "db::store": [
                {"key": "k1", "value": {"x": 1}},
                {"key": "k2"}
            ],
            "db::not-array": "x"
        });

        let validated = validate_indexeddb_export(raw);
        let store = validated.get("db::store").expect("store exists");
        assert_eq!(store.len(), 1);
        assert_eq!(store[0].key, json!("k1"));
    }
}
