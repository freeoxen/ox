//! Re-export the upstream StructFS store combinator.

pub use structfs_core_store::Masked;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::path;
    use structfs_core_store::{Error as StoreError, Path, PathPattern, Reader, Record, Value};

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
        data.insert("gate/model".to_string(), Value::String("gpt-4o".into()));
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
        let mut masked = Masked::with_mask(
            test_store(),
            vec![PathPattern::prefix(path!("gate/api_key"))],
            Value::String("***".into()),
        );
        let result = masked.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("gpt-4o".into()));
    }

    #[test]
    fn masked_path_returns_mask_value() {
        let mut masked = Masked::with_mask(
            test_store(),
            vec![PathPattern::prefix(path!("gate/api_key"))],
            Value::String("***".into()),
        );
        let result = masked.read(&path!("gate/api_key")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("***".into()));
    }

    #[test]
    fn masked_nonexistent_returns_none() {
        let mut masked = Masked::with_mask(
            test_store(),
            vec![PathPattern::prefix(path!("gate/api_key"))],
            Value::String("***".into()),
        );
        let result = masked.read(&Path::parse("nonexistent").unwrap()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn multiple_masked_paths() {
        let mut masked = Masked::with_mask(
            test_store(),
            vec![
                PathPattern::prefix(path!("gate/api_key")),
                PathPattern::prefix(path!("gate/model")),
            ],
            Value::String("REDACTED".into()),
        );
        let key = masked.read(&path!("gate/api_key")).unwrap().unwrap();
        assert_eq!(key.as_value().unwrap(), &Value::String("REDACTED".into()));
        let model = masked.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(model.as_value().unwrap(), &Value::String("REDACTED".into()));
        // Unmasked still works
        let provider = masked.read(&path!("gate/provider")).unwrap().unwrap();
        assert_eq!(
            provider.as_value().unwrap(),
            &Value::String("anthropic".into())
        );
    }
}
