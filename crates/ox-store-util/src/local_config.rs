//! LocalConfig — in-memory path-based Reader/Writer for standalone config.
//!
//! Used by ox-web (no broker) and tests. Values are stored in a flat
//! BTreeMap keyed by path strings.

use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// In-memory config store implementing Reader and Writer.
pub struct LocalConfig {
    values: BTreeMap<String, Value>,
}

impl LocalConfig {
    pub fn new() -> Self {
        Self {
            values: BTreeMap::new(),
        }
    }

    /// Set a value at a path (convenience for construction).
    pub fn set(&mut self, path: &str, value: Value) {
        self.values.insert(path.to_string(), value);
    }
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader for LocalConfig {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();
        // Exact match
        if let Some(val) = self.values.get(&key) {
            return Ok(Some(Record::parsed(val.clone())));
        }
        // If reading root, return all values as a map
        if from.is_empty() {
            let mut map = BTreeMap::new();
            for (k, v) in &self.values {
                map.insert(k.clone(), v.clone());
            }
            return Ok(Some(Record::parsed(Value::Map(map))));
        }
        Ok(None)
    }
}

impl Writer for LocalConfig {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = to.to_string();
        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("LocalConfig", "write", "expected parsed value"))?
            .clone();
        self.values.insert(key, value);
        Ok(to.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn read_empty_returns_none() {
        let mut config = LocalConfig::new();
        let result = config.read(&path!("gate/model")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn set_then_read() {
        let mut config = LocalConfig::new();
        config.set("gate/model", Value::String("gpt-4o".into()));
        let result = config.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("gpt-4o".into()));
    }

    #[test]
    fn write_then_read() {
        let mut config = LocalConfig::new();
        config
            .write(
                &path!("gate/provider"),
                Record::parsed(Value::String("openai".into())),
            )
            .unwrap();
        let result = config.read(&path!("gate/provider")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("openai".into()));
    }

    #[test]
    fn read_root_returns_all() {
        let mut config = LocalConfig::new();
        config.set("gate/model", Value::String("gpt-4o".into()));
        config.set("gate/provider", Value::String("openai".into()));
        let result = config
            .read(&Path::from_components(vec![]))
            .unwrap()
            .unwrap();
        match result.as_value().unwrap() {
            Value::Map(m) => {
                assert_eq!(m.len(), 2);
                assert!(m.contains_key("gate/model"));
                assert!(m.contains_key("gate/provider"));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn write_overwrites_existing() {
        let mut config = LocalConfig::new();
        config.set("gate/model", Value::String("old".into()));
        config
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("new".into())),
            )
            .unwrap();
        let result = config.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("new".into()));
    }
}
