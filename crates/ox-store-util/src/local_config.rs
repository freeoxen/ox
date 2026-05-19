//! LocalConfig — in-memory path-based Reader/Writer for standalone config.
//!
//! Used by ox-web (no broker) and tests. Values are stored in a flat
//! BTreeMap keyed by path strings.

use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::flatten_value_into;

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
        // StructFS convention: a path is either a leaf value OR a Map of
        // immediate children. Read returns whatever sits at that point in
        // the tree-of-Maps projection. Well-formed data never has both at
        // the same point; an explicit error guards the malformed case.
        let key = from.to_string();
        let has_leaf = !key.is_empty() && self.values.contains_key(&key);

        let child_prefix = if key.is_empty() {
            String::new()
        } else {
            format!("{key}/")
        };
        let has_children = self
            .values
            .keys()
            .any(|k| k.starts_with(&child_prefix) && *k != key);

        if has_leaf && has_children {
            return Err(StoreError::store(
                "LocalConfig",
                "read",
                format!(
                    "malformed store: path {key:?} has both a leaf value and child entries"
                ),
            ));
        }
        if has_leaf {
            return Ok(Some(Record::parsed(self.values[&key].clone())));
        }
        if !has_children {
            return Ok(None);
        }

        // Collect the unique immediate-child segments under `from`, then
        // recurse on each to assemble its sub-Map. Recursion drives the
        // leaf-vs-children distinction at every depth without needing to
        // hold a borrow on `self.values` across calls.
        let mut heads: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for full_key in self.values.keys() {
            let Some(rest) = full_key.strip_prefix(&child_prefix) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let head = match rest.split_once('/') {
                Some((h, _)) => h.to_string(),
                None => rest.to_string(),
            };
            heads.insert(head);
        }

        let mut children: BTreeMap<String, Value> = BTreeMap::new();
        for head in heads {
            let sub_path_str = if child_prefix.is_empty() {
                head.clone()
            } else {
                format!("{child_prefix}{head}")
            };
            let sub_path = Path::parse(&sub_path_str)?;
            let Some(rec) = self.read(&sub_path)? else {
                continue;
            };
            let child_value = rec.as_value().cloned().ok_or_else(|| {
                StoreError::store("LocalConfig", "read", "child record had no value")
            })?;
            children.insert(head, child_value);
        }

        Ok(Some(Record::parsed(Value::Map(children))))
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

        // StructFS convention: a path is either a leaf or a Map of
        // children — never both. A Map at a parent path means "this is
        // the full state under here", so existing entries under the
        // prefix (including a prior leaf at the same key) are stale
        // and must be cleared before the Map's leaves go in. Flatten
        // the Map into flat sub-keys to preserve the leaf-only shape
        // the read side enforces.
        if matches!(value, Value::Map(_)) {
            let descendant_prefix = format!("{}/", key);
            self.values
                .retain(|k, _| k != &key && !k.starts_with(&descendant_prefix));
            // Flatten after the retain: the inserts must follow the sweep
            // so the new Map's leaves aren't swept away with the stale ones.
            flatten_value_into(&key, &value, &mut self.values);
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
    fn read_root_returns_immediate_children_as_nested_maps() {
        // Root read projects the tree-of-Maps: top-level keys map to
        // sub-Maps, recursing through each level.
        let mut config = LocalConfig::new();
        config.set("gate/model", Value::String("gpt-4o".into()));
        config.set("gate/provider", Value::String("openai".into()));
        let result = config
            .read(&Path::from_components(vec![]))
            .unwrap()
            .unwrap();
        match result.as_value().unwrap() {
            Value::Map(m) => {
                assert_eq!(m.len(), 1, "root has one immediate child: {m:?}");
                let gate = match m.get("gate").expect("gate present") {
                    Value::Map(g) => g,
                    other => panic!("expected gate to be a Map; got {other:?}"),
                };
                assert_eq!(gate.len(), 2);
                assert_eq!(gate.get("model"), Some(&Value::String("gpt-4o".into())));
                assert_eq!(gate.get("provider"), Some(&Value::String("openai".into())));
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

        // Child enumeration of `parent` returns only K2's segment.
        // Reading at the prefix path now returns a Value::Map of immediate
        // children directly — no manual walk needed.
        let rec = config
            .read(&path!("parent"))
            .unwrap()
            .expect("parent has children");
        let segments: Vec<String> = match rec.as_value().unwrap() {
            Value::Map(m) => m.keys().cloned().collect(),
            other => panic!("expected Map at parent; got {other:?}"),
        };
        assert_eq!(segments, vec!["k1_other".to_string()]);
    }

    #[test]
    fn read_at_non_leaf_returns_value_map_of_immediate_children() {
        let mut cfg = LocalConfig::new();
        cfg.set("settings/index/entries/accounts", Value::String("acc".into()));
        cfg.set("settings/index/entries/models", Value::String("mod".into()));
        cfg.set("settings/other", Value::String("other".into()));

        let path = Path::parse("settings/index/entries").unwrap();
        let rec = cfg.read(&path).unwrap().expect("non-leaf returns Some");
        let value = rec.as_value().expect("non-leaf has a value");
        let map = match value {
            Value::Map(m) => m.clone(),
            other => panic!("expected Map at non-leaf path; got {other:?}"),
        };
        assert!(map.contains_key("accounts"), "accounts child missing; map={map:?}");
        assert!(map.contains_key("models"), "models child missing; map={map:?}");
        assert_eq!(map.len(), 2, "more than immediate children; map={map:?}");
    }

    #[test]
    fn read_at_leaf_still_returns_the_leaf_value() {
        let mut cfg = LocalConfig::new();
        cfg.set("settings/index/entries/accounts", Value::String("acc".into()));

        let path = Path::parse("settings/index/entries/accounts").unwrap();
        let rec = cfg.read(&path).unwrap().expect("leaf returns Some");
        let value = rec.as_value().expect("leaf has a value");
        assert_eq!(value, &Value::String("acc".into()));
    }

    #[test]
    fn read_at_missing_path_returns_none() {
        let mut cfg = LocalConfig::new();
        cfg.set("settings/index/entries/accounts", Value::String("acc".into()));

        let path = Path::parse("not/a/real/prefix").unwrap();
        let rec = cfg.read(&path).unwrap();
        assert!(rec.is_none(), "missing path must return None");
    }

    #[test]
    fn nested_non_leaf_reads_return_nested_maps() {
        let mut cfg = LocalConfig::new();
        cfg.set("a/b/c/d", Value::String("d-val".into()));
        cfg.set("a/b/c/e", Value::String("e-val".into()));
        cfg.set("a/b/x", Value::String("x-val".into()));

        let rec = cfg
            .read(&Path::parse("a").unwrap())
            .unwrap()
            .expect("non-leaf returns Some");
        let map = match rec.as_value().unwrap() {
            Value::Map(m) => m.clone(),
            other => panic!("expected Map; got {other:?}"),
        };
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("b"));
    }

    #[test]
    fn write_map_at_parent_replaces_stale_sub_keys_with_flat_leaves() {
        let mut cfg = LocalConfig::new();
        cfg.set("gate/lm/dialect", Value::String("stale".into()));
        cfg.set("gate/lm/endpoint", Value::String("stale".into()));
        cfg.set("gate/lm/auth", Value::String("stale".into()));

        let mut new_map = BTreeMap::new();
        new_map.insert("dialect".to_string(), Value::String("openai".into()));
        new_map.insert("endpoint".to_string(), Value::String("http://x".into()));
        cfg.write(&path!("gate/lm"), Record::parsed(Value::Map(new_map)))
            .unwrap();

        // Sub-keys present in the new Map carry its values, as flat leaves.
        assert_eq!(
            cfg.read(&path!("gate/lm/dialect")).unwrap().unwrap().as_value(),
            Some(&Value::String("openai".into())),
        );
        assert_eq!(
            cfg.read(&path!("gate/lm/endpoint")).unwrap().unwrap().as_value(),
            Some(&Value::String("http://x".into())),
        );
        // Sub-keys absent from the new Map are gone — the Map's intent is
        // "this is the full state under here", not a partial overlay.
        assert!(cfg.read(&path!("gate/lm/auth")).unwrap().is_none());
        // Reading the parent returns the children-Map view, not a leaf:
        // the write normalized storage to the flat-leaf shape.
        let rec = cfg
            .read(&path!("gate/lm"))
            .unwrap()
            .expect("parent has children");
        let map = match rec.as_value().unwrap() {
            Value::Map(m) => m.clone(),
            other => panic!("expected Map; got {other:?}"),
        };
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("dialect"));
        assert!(map.contains_key("endpoint"));
    }
}
