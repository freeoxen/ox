//! Compatibility adapter for paths serialized as component arrays.
//!
//! StructFS 0.2 implements Serde for `Path` using slash-separated strings.
//! Existing Ox/Horns records use arrays, so keep this explicit adapter at
//! those boundaries instead of changing their stored or transmitted shape.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use structfs_core_store::{Path, Value};

/// Encode a path using the same component-array shape in a parsed record.
pub fn to_value(path: &Path) -> Value {
    Value::Array(
        path.iter()
            .map(|part| Value::String(part.to_owned()))
            .collect(),
    )
}

pub fn serialize<S: Serializer>(path: &Path, ser: S) -> Result<S::Ok, S::Error> {
    let components: Vec<&str> = path.iter().collect();
    components.serialize(ser)
}

pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Path, D::Error> {
    let components = Vec::<String>::deserialize(de)?;
    Path::try_from_components(components).map_err(serde::de::Error::custom)
}

/// Adapter for `Option<Path>` fields.
#[allow(dead_code)]
pub mod option {
    use super::*;

    pub fn serialize<S: Serializer>(path: &Option<Path>, ser: S) -> Result<S::Ok, S::Error> {
        match path {
            Some(p) => {
                let components: Vec<&str> = p.iter().collect();
                ser.serialize_some(&components)
            }
            None => ser.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Path>, D::Error> {
        let opt = Option::<Vec<String>>::deserialize(de)?;
        match opt {
            Some(components) => Path::try_from_components(components)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
