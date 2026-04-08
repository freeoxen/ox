//! Masked — path-based masking wrapper that redacts specified paths on read.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value};

/// Wraps a Reader, returning a mask value for specified paths.
///
/// Use this to hide sensitive data (API keys) when exposing config
/// to display layers.
pub struct Masked<S> {
    inner: S,
    masked_paths: Vec<String>,
    mask_value: Value,
}

impl<S> Masked<S> {
    /// Create a Masked wrapper.
    ///
    /// `masked_paths` are path strings to match against. A read path
    /// matches if it starts with any masked path.
    pub fn new(inner: S, masked_paths: Vec<String>, mask_value: Value) -> Self {
        Self {
            inner,
            masked_paths,
            mask_value,
        }
    }

    fn is_masked(&self, path: &Path) -> bool {
        let path_str = path.to_string();
        self.masked_paths
            .iter()
            .any(|masked| path_str.starts_with(masked))
    }
}

impl<S: Reader> Reader for Masked<S> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if self.is_masked(from) {
            // Only return mask if the underlying value exists
            match self.inner.read(from)? {
                Some(_) => Ok(Some(Record::parsed(self.mask_value.clone()))),
                None => Ok(None),
            }
        } else {
            self.inner.read(from)
        }
    }
}

unsafe impl<S: Send> Send for Masked<S> {}
unsafe impl<S: Sync> Sync for Masked<S> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::path;

    struct MapStore {
        data: BTreeMap<String, Value>,
    }

    impl Reader for MapStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            let key = from.to_string();
            Ok(self.data.get(&key).map(|v| Record::parsed(v.clone())))
        }
    }

    fn test_store() -> MapStore {
        let mut data = BTreeMap::new();
        data.insert("model/id".to_string(), Value::String("gpt-4o".into()));
        data.insert(
            "gate/api_key".to_string(),
            Value::String("sk-secret".into()),
        );
        data.insert(
            "gate/provider".to_string(),
            Value::String("anthropic".into()),
        );
        MapStore { data }
    }

    #[test]
    fn unmasked_path_passes_through() {
        let mut masked = Masked::new(
            test_store(),
            vec!["gate/api_key".into()],
            Value::String("***".into()),
        );
        let result = masked.read(&path!("model/id")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("gpt-4o".into()));
    }

    #[test]
    fn masked_path_returns_mask_value() {
        let mut masked = Masked::new(
            test_store(),
            vec!["gate/api_key".into()],
            Value::String("***".into()),
        );
        let result = masked.read(&path!("gate/api_key")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("***".into()));
    }

    #[test]
    fn masked_nonexistent_returns_none() {
        let mut masked = Masked::new(
            test_store(),
            vec!["gate/api_key".into()],
            Value::String("***".into()),
        );
        let result = masked.read(&Path::parse("nonexistent").unwrap()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn multiple_masked_paths() {
        let mut masked = Masked::new(
            test_store(),
            vec!["gate/api_key".into(), "model/id".into()],
            Value::String("REDACTED".into()),
        );
        let key = masked.read(&path!("gate/api_key")).unwrap().unwrap();
        assert_eq!(key.as_value().unwrap(), &Value::String("REDACTED".into()));
        let model = masked.read(&path!("model/id")).unwrap().unwrap();
        assert_eq!(model.as_value().unwrap(), &Value::String("REDACTED".into()));
        // Unmasked still works
        let provider = masked.read(&path!("gate/provider")).unwrap().unwrap();
        assert_eq!(
            provider.as_value().unwrap(),
            &Value::String("anthropic".into())
        );
    }
}
