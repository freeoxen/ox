//! Compatibility adapter for paths serialized as component arrays.
//!
//! StructFS implements Serde for `Path` using slash-separated strings.
//! Existing Ox/Horns records use arrays, so keep this explicit adapter at
//! those boundaries instead of changing their stored or transmitted shape.
//! The component-array adapters are supplied by StructFS 0.3.

pub use structfs_core_store::path_serde::components::{deserialize, serialize};
pub use structfs_core_store::path_serde::optional_components as option;
use structfs_core_store::{Path, Value};

/// Encode a path using the same component-array shape in a parsed record.
pub fn to_value(path: &Path) -> Value {
    Value::Array(
        path.iter()
            .map(|part| Value::String(part.to_owned()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use structfs_core_store::path;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct LegacyPaths {
        #[serde(with = "super")]
        required: Path,
        #[serde(with = "super::option")]
        optional: Option<Path>,
    }

    #[test]
    fn component_arrays_remain_compatible_with_existing_records() {
        for optional in [Some(path!("users/123")), None] {
            let paths = LegacyPaths {
                required: path!("settings/accounts"),
                optional,
            };
            let expected = serde_json::json!({
                "required": ["settings", "accounts"],
                "optional": paths.optional.as_ref().map(|_| vec!["users", "123"])
            });
            assert_eq!(serde_json::to_value(&paths).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<LegacyPaths>(expected).unwrap(),
                paths
            );
        }
        let invalid = serde_json::json!({"required": ["bad-name"], "optional": null});
        assert!(serde_json::from_value::<LegacyPaths>(invalid).is_err());
    }
}
