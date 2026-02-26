use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::debug;
use serde_json::{Map, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub fn ensure_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("Path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create parent directory: {}", parent.display()))
}

pub fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    debug!("Reading JSON file: {}", path.display());
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let value: Value = serde_json::from_reader(reader)
        .with_context(|| format!("Failed to parse JSON file {}", path.display()))?;
    let object = value.as_object().cloned().with_context(|| {
        format!(
            "Expected a JSON object in {} but found {}",
            path.display(),
            type_name(&value)
        )
    })?;
    Ok(object)
}

pub fn write_json_object(path: &Path, payload: &Map<String, Value>) -> Result<()> {
    ensure_parent(path)?;
    debug!("Writing JSON file: {}", path.display());
    let mut file =
        File::create(path).with_context(|| format!("Failed to create {}", path.display()))?;
    let sorted = sort_object(payload);
    let rendered = serde_json::to_string_pretty(&Value::Object(sorted))
        .with_context(|| format!("Failed to serialize JSON for {}", path.display()))?;
    file.write_all(rendered.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("Failed to finalize {}", path.display()))?;
    Ok(())
}

pub fn parse_int_timestamp(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => {
            if let Some(as_i64) = number.as_i64() {
                return Some(as_i64);
            }
            number.as_f64().map(|as_f64| as_f64 as i64)
        }
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Ok(parsed) = trimmed.parse::<i64>() {
                return Some(parsed);
            }
            parse_iso_timestamp(trimmed)
        }
        _ => None,
    }
}

fn parse_iso_timestamp(raw: &str) -> Option<i64> {
    let normalized = raw.replace('Z', "+00:00");
    DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc).timestamp_millis())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

pub fn sha1_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut digest = Sha1::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

pub fn sha256_text(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    hex::encode(digest.finalize())
}

pub fn sha1_text(text: &str) -> String {
    let mut digest = Sha1::new();
    digest.update(text.as_bytes());
    hex::encode(digest.finalize())
}

pub fn conflict_path(path: &Path, source_label: &str, file_hash: &str) -> PathBuf {
    let suffix = format!("__{source_label}_{}", &file_hash[..8.min(file_hash.len())]);
    match (path.file_stem(), path.extension()) {
        (Some(stem), Some(ext)) => {
            let name = format!(
                "{}{}.{}",
                stem.to_string_lossy(),
                suffix,
                ext.to_string_lossy()
            );
            path.with_file_name(name)
        }
        _ => {
            let name = format!(
                "{}{}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                suffix
            );
            path.with_file_name(name)
        }
    }
}

fn sort_object(payload: &Map<String, Value>) -> Map<String, Value> {
    let mut sorted = Map::new();
    let mut keys: Vec<&String> = payload.keys().collect();
    keys.sort();
    for key in keys {
        let value = payload.get(key).cloned().unwrap_or(Value::Null);
        sorted.insert(key.clone(), sort_value(value));
    }
    sorted
}

fn sort_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(sort_object(&map)),
        Value::Array(values) => Value::Array(values.into_iter().map(sort_value).collect()),
        other => other,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
