use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::utils::parse_int_timestamp;

pub fn merge_session_metadata(
    record_a: &Map<String, Value>,
    record_b: &Map<String, Value>,
) -> Map<String, Value> {
    let newer = pick_newer_record(record_a, record_b);
    let older = if std::ptr::eq(newer, record_a) {
        record_b
    } else {
        record_a
    };

    let mut merged = newer.clone();
    merged.insert(
        "createdAt".to_string(),
        pick_created_at(record_a, record_b)
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    merged.insert(
        "lastActivityAt".to_string(),
        Value::from(pick_last_activity(record_a, record_b)),
    );

    merge_simple_unions(&mut merged, record_a, record_b);
    merge_fs_detected_files(&mut merged, record_a, record_b);
    merge_newer_wins_maps(&mut merged, older, newer);
    merged
}

fn pick_newer_record<'a>(
    record_a: &'a Map<String, Value>,
    record_b: &'a Map<String, Value>,
) -> &'a Map<String, Value> {
    let last_a = pick_last_activity(record_a, &Map::new());
    let last_b = pick_last_activity(record_b, &Map::new());
    if last_b > last_a {
        record_b
    } else {
        record_a
    }
}

fn pick_created_at(record_a: &Map<String, Value>, record_b: &Map<String, Value>) -> Option<i64> {
    let values = [record_a.get("createdAt"), record_b.get("createdAt")];
    values
        .into_iter()
        .flatten()
        .filter_map(parse_int_timestamp)
        .min()
}

fn pick_last_activity(record_a: &Map<String, Value>, record_b: &Map<String, Value>) -> i64 {
    let values = [
        record_a.get("lastActivityAt"),
        record_b.get("lastActivityAt"),
    ];
    values
        .into_iter()
        .flatten()
        .filter_map(parse_int_timestamp)
        .max()
        .unwrap_or(0)
}

fn merge_simple_unions(
    merged: &mut Map<String, Value>,
    record_a: &Map<String, Value>,
    record_b: &Map<String, Value>,
) {
    merged.insert(
        "userApprovedFileAccessPaths".to_string(),
        Value::Array(merge_distinct_lists(
            record_a.get("userApprovedFileAccessPaths"),
            record_b.get("userApprovedFileAccessPaths"),
        )),
    );
}

fn merge_distinct_lists(value_a: Option<&Value>, value_b: Option<&Value>) -> Vec<Value> {
    let mut merged = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for candidate in [value_a, value_b] {
        let Some(Value::Array(items)) = candidate else {
            continue;
        };
        for item in items {
            let marker = serde_json::to_string(item)
                .unwrap_or_else(|_| String::from("<serialization-error>"));
            if seen.insert(marker) {
                merged.push(item.clone());
            }
        }
    }
    merged
}

fn merge_fs_detected_files(
    merged: &mut Map<String, Value>,
    record_a: &Map<String, Value>,
    record_b: &Map<String, Value>,
) {
    let mut by_host: HashMap<String, Map<String, Value>> = HashMap::new();
    for item in iter_fs_detected_files(record_a)
        .into_iter()
        .chain(iter_fs_detected_files(record_b).into_iter())
    {
        let host_path = item
            .get("hostPath")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match by_host.get(&host_path) {
            Some(current) => {
                let current_ts = current
                    .get("timestamp")
                    .and_then(parse_int_timestamp)
                    .unwrap_or(0);
                let candidate_ts = item
                    .get("timestamp")
                    .and_then(parse_int_timestamp)
                    .unwrap_or(0);
                if candidate_ts >= current_ts {
                    by_host.insert(host_path, item);
                }
            }
            None => {
                by_host.insert(host_path, item);
            }
        }
    }
    let mut values: Vec<Value> = by_host.into_values().map(Value::Object).collect();
    values.sort_by_key(|value| {
        value
            .as_object()
            .and_then(|obj| obj.get("hostPath"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    });
    merged.insert("fsDetectedFiles".to_string(), Value::Array(values));
}

fn iter_fs_detected_files(record: &Map<String, Value>) -> Vec<Map<String, Value>> {
    let Some(Value::Array(items)) = record.get("fsDetectedFiles") else {
        return Vec::new();
    };
    items.iter().filter_map(Value::as_object).cloned().collect()
}

fn merge_newer_wins_maps(
    merged: &mut Map<String, Value>,
    older: &Map<String, Value>,
    newer: &Map<String, Value>,
) {
    for field in ["mcqAnswers", "enabledMcpTools"] {
        let older_value = older
            .get(field)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let newer_value = newer
            .get(field)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        merged.insert(
            field.to_string(),
            Value::Object(deep_merge_dicts(&older_value, &newer_value)),
        );
    }
}

fn deep_merge_dicts(
    base: &Map<String, Value>,
    override_map: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = base.clone();
    for (key, value) in override_map {
        match (merged.get(key), value) {
            (Some(Value::Object(current)), Value::Object(next)) => {
                merged.insert(key.clone(), Value::Object(deep_merge_dicts(current, next)));
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map, Value};

    use super::merge_session_metadata;

    #[test]
    fn merge_session_metadata_prefers_newer_record_and_unions_fields() {
        let a = as_map(json!({
            "createdAt": 100,
            "lastActivityAt": 200,
            "title": "old",
            "userApprovedFileAccessPaths": ["/tmp/a"],
            "fsDetectedFiles": [{"hostPath": "/tmp/a.txt", "timestamp": 10, "fileName": "a"}],
            "mcqAnswers": {"q1": {"choice": "A"}},
            "enabledMcpTools": {"tool_a": true}
        }));
        let b = as_map(json!({
            "createdAt": 150,
            "lastActivityAt": 300,
            "title": "new",
            "userApprovedFileAccessPaths": ["/tmp/b"],
            "fsDetectedFiles": [{"hostPath": "/tmp/a.txt", "timestamp": 20, "fileName": "b"}],
            "mcqAnswers": {"q1": {"choice": "B"}, "q2": {"choice": "C"}},
            "enabledMcpTools": {"tool_b": true}
        }));

        let merged = merge_session_metadata(&a, &b);

        assert_eq!(merged.get("createdAt"), Some(&Value::from(100)));
        assert_eq!(merged.get("lastActivityAt"), Some(&Value::from(300)));
        assert_eq!(merged.get("title"), Some(&Value::from("new")));
        let paths = merged
            .get("userApprovedFileAccessPaths")
            .and_then(Value::as_array)
            .expect("paths array");
        assert_eq!(paths.len(), 2);
        let files = merged
            .get("fsDetectedFiles")
            .and_then(Value::as_array)
            .expect("fs files array");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["fileName"], Value::from("b"));
        assert_eq!(merged["mcqAnswers"]["q1"]["choice"], Value::from("B"));
        assert_eq!(merged["mcqAnswers"]["q2"]["choice"], Value::from("C"));
        assert_eq!(merged["enabledMcpTools"]["tool_a"], Value::from(true));
        assert_eq!(merged["enabledMcpTools"]["tool_b"], Value::from(true));
    }

    fn as_map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }
}
