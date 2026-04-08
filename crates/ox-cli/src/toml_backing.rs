//! TomlFileBacking — persists flat path-keyed BTreeMap as nested TOML.

use std::collections::BTreeMap;
use std::path::PathBuf;
use structfs_core_store::{Error as StoreError, Value};

pub struct TomlFileBacking {
    path: PathBuf,
}

impl TomlFileBacking {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ox_store_util::StoreBacking for TomlFileBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::store("toml_backing", "load", e.to_string())),
        };
        let table: toml::Table = content.parse().map_err(|e: toml::de::Error| {
            StoreError::store("toml_backing", "load", e.to_string())
        })?;
        let mut flat = BTreeMap::new();
        flatten_toml("", &toml::Value::Table(table), &mut flat);
        Ok(Some(Value::Map(flat)))
    }

    fn save(&self, value: &Value) -> Result<(), StoreError> {
        let Value::Map(flat) = value else {
            return Err(StoreError::store(
                "toml_backing",
                "save",
                "expected Value::Map",
            ));
        };
        let mut root = toml::Table::new();
        for (path_key, val) in flat {
            let parts: Vec<&str> = path_key.split('/').collect();
            insert_nested(&mut root, &parts, val);
        }
        let content = toml::to_string_pretty(&root)
            .map_err(|e| StoreError::store("toml_backing", "save", e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::store("toml_backing", "save", e.to_string()))?;
        }
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, content)
            .map_err(|e| StoreError::store("toml_backing", "save", e.to_string()))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| StoreError::store("toml_backing", "save", e.to_string()))?;
        Ok(())
    }
}

fn flatten_toml(prefix: &str, value: &toml::Value, out: &mut BTreeMap<String, Value>) {
    match value {
        toml::Value::Table(table) => {
            for (key, val) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}/{key}")
                };
                flatten_toml(&path, val, out);
            }
        }
        toml::Value::String(s) => {
            out.insert(prefix.to_string(), Value::String(s.clone()));
        }
        toml::Value::Integer(n) => {
            out.insert(prefix.to_string(), Value::Integer(*n));
        }
        toml::Value::Boolean(b) => {
            out.insert(prefix.to_string(), Value::Bool(*b));
        }
        _ => {}
    }
}

fn insert_nested(table: &mut toml::Table, parts: &[&str], value: &Value) {
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 {
        match value {
            Value::String(s) => {
                table.insert(parts[0].to_string(), toml::Value::String(s.clone()));
            }
            Value::Integer(n) => {
                table.insert(parts[0].to_string(), toml::Value::Integer(*n));
            }
            Value::Bool(b) => {
                table.insert(parts[0].to_string(), toml::Value::Boolean(*b));
            }
            _ => {}
        }
        return;
    }
    let sub = table
        .entry(parts[0].to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(sub_table) = sub {
        insert_nested(sub_table, &parts[1..], value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_store_util::StoreBacking;

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backing = TomlFileBacking::new(path.clone());
        assert!(backing.load().unwrap().is_none());
        let mut map = BTreeMap::new();
        map.insert("gate/model".to_string(), Value::String("gpt-4o".into()));
        map.insert("gate/max_tokens".to_string(), Value::Integer(8192));
        map.insert("gate/provider".to_string(), Value::String("openai".into()));
        backing.save(&Value::Map(map)).unwrap();
        assert!(path.exists());
        let loaded = backing.load().unwrap().unwrap();
        match loaded {
            Value::Map(m) => {
                assert_eq!(
                    m.get("gate/model").unwrap(),
                    &Value::String("gpt-4o".into())
                );
                assert_eq!(m.get("gate/max_tokens").unwrap(), &Value::Integer(8192));
                assert_eq!(
                    m.get("gate/provider").unwrap(),
                    &Value::String("openai".into())
                );
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn toml_file_is_human_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backing = TomlFileBacking::new(path.clone());
        let mut map = BTreeMap::new();
        map.insert("gate/model".to_string(), Value::String("gpt-4o".into()));
        map.insert("gate/max_tokens".to_string(), Value::Integer(8192));
        backing.save(&Value::Map(map)).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("[gate]"),
            "expected [gate] section, got:\n{content}"
        );
        assert!(content.contains("gpt-4o"));
    }
}
