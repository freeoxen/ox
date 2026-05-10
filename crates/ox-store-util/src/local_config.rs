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

        // StructFS convention: writing `Value::Null` deletes the element
        // AND its subtree. A flat-keyed map can't represent "missing"
        // distinctly from "Null-tombstoned" without retaining the keys,
        // so subsequent reads have to return Ok(None) for the target
        // and every descendant. Achieve that by removing the keys
        // outright — there is no base layer underneath us, so removal
        // is the natural representation of "gone".
        //
        // Component-aware prefix: a write to `accounts` must remove
        // `accounts/foo` but leave `accounts_other/bar` alone. The
        // `<key>/` separator check makes the prefix component-aligned.
        if matches!(value, Value::Null) {
            if key.is_empty() {
                self.values.clear();
            } else {
                let descendant_prefix = format!("{}/", key);
                self.values
                    .retain(|k, _| k != &key && !k.starts_with(&descendant_prefix));
            }
            return Ok(to.clone());
        }

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

    #[test]
    fn null_write_cascades_subtree_and_spares_siblings() {
        // StructFS convention: writing `Value::Null` at K1 deletes K1
        // AND every descendant whose key extends K1 component-aligned.
        // Sibling top-level keys are untouched. Child enumeration of
        // the parent of K1+K2 must report only K2's segment afterwards.
        //
        // The component-aware prefix matters: `accounts` must not
        // accidentally remove `accounts_other`. We exercise that by
        // making K2 a sibling whose key shares a string prefix with K1
        // up to the boundary character (path separator vs. underscore).
        let mut config = LocalConfig::new();

        // K1 = `parent/k1`, with a deep subtree.
        config.set("parent/k1", Value::String("v".into()));
        config.set("parent/k1/sub1", Value::String("v".into()));
        config.set("parent/k1/sub2", Value::String("v".into()));
        config.set("parent/k1/sub2/deep", Value::String("v".into()));

        // K2 = `parent/k1_other` — sibling at the same depth whose
        // string starts with `parent/k1`. Component-aware matching
        // protects this from the cascade.
        config.set("parent/k1_other", Value::String("sibling".into()));

        config
            .write(&path!("parent/k1"), Record::parsed(Value::Null))
            .unwrap();

        // K1 itself and every descendant must read as gone.
        assert!(config.read(&path!("parent/k1")).unwrap().is_none());
        assert!(config.read(&path!("parent/k1/sub1")).unwrap().is_none());
        assert!(config.read(&path!("parent/k1/sub2")).unwrap().is_none());
        assert!(
            config
                .read(&path!("parent/k1/sub2/deep"))
                .unwrap()
                .is_none()
        );

        // Sibling untouched.
        assert_eq!(
            config
                .read(&path!("parent/k1_other"))
                .unwrap()
                .unwrap()
                .as_value()
                .unwrap(),
            &Value::String("sibling".into()),
        );

        // Child enumeration of `parent` returns only K2's segment. The
        // store has no first-class enumeration API; the renderer's
        // `child_names_under` walks the root-read Map's keys, so we
        // mirror that traversal here.
        let root = config
            .read(&Path::from_components(vec![]))
            .unwrap()
            .unwrap();
        let mut segments: Vec<String> = Vec::new();
        if let Value::Map(m) = root.as_value().unwrap() {
            let prefix = "parent/";
            for key in m.keys() {
                if let Some(rest) = key.strip_prefix(prefix) {
                    let segment = match rest.split('/').next() {
                        Some(s) if !s.is_empty() => s.to_string(),
                        _ => continue,
                    };
                    if !segments.contains(&segment) {
                        segments.push(segment);
                    }
                }
            }
        }
        assert_eq!(segments, vec!["k1_other".to_string()]);
    }
}
