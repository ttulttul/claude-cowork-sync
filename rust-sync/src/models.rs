use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoworkReadState {
    #[serde(default)]
    pub sessions: HashMap<String, i64>,
    #[serde(default, rename = "initializedAt")]
    pub initialized_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexedDbRecord {
    pub key: Value,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserStateExport {
    #[serde(default = "default_schema_version", rename = "schemaVersion")]
    pub schema_version: String,
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(rename = "exportedAt")]
    pub exported_at: i64,
    #[serde(default, rename = "localStorage")]
    pub local_storage: HashMap<String, String>,
    #[serde(default, rename = "indexedDb")]
    pub indexed_db: HashMap<String, Vec<IndexedDbRecord>>,
}

#[derive(Debug, Clone)]
pub struct SessionSourceRecord {
    pub source_label: String,
    pub session_id: String,
    pub profile_dir: PathBuf,
    pub json_path: PathBuf,
    pub folder_path: PathBuf,
    pub relative_group_dir: PathBuf,
    pub metadata: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBinding {
    pub session_id: String,
    pub last_activity_at: i64,
    pub cli_session_id: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionMergeResult {
    pub session_id: String,
    pub json_path: PathBuf,
    pub folder_path: PathBuf,
    pub binding: SessionBinding,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ValidationResult {
    #[serde(rename = "missingSessionFolders")]
    pub missing_session_folders: Vec<String>,
    #[serde(rename = "missingCliBindingKeys")]
    pub missing_cli_binding_keys: Vec<String>,
    #[serde(rename = "missingCoworkReadStateSessions")]
    pub missing_cowork_read_state_sessions: Vec<String>,
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        self.missing_session_folders.is_empty()
            && self.missing_cli_binding_keys.is_empty()
            && self.missing_cowork_read_state_sessions.is_empty()
    }
}

pub type SessionMap = BTreeMap<String, SessionMergeResult>;

fn default_schema_version() -> String {
    "1".to_string()
}

fn default_origin() -> String {
    "https://claude.ai".to_string()
}
