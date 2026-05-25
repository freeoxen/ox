//! Serde adapter for `structfs_core_store::Path`.
//!
//! `Path` itself doesn't implement `Serialize`/`Deserialize` (the upstream
//! crate doesn't expose a serde feature), but our cross-boundary records
//! (`SettingsIndexEntry`, `BindingEntry`, `PathPattern`) all carry `Path`
//! fields. We serialize a `Path` as its component vector — round-tripping
//! through `Path::try_from_components` so deserialization re-validates each
//! component.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use structfs_core_store::Path;

pub fn serialize<S: Serializer>(path: &Path, ser: S) -> Result<S::Ok, S::Error> {
    let components: Vec<&String> = path.iter().collect();
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
                let components: Vec<&String> = p.iter().collect();
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
