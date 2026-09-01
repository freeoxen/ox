use structfs_core_store::Error as StoreError;

pub(crate) fn encode_id(id: &str) -> String {
    let mut encoded = String::with_capacity(1 + id.len() * 2);
    encoded.push('i');
    for byte in id.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub(crate) fn decode_id(
    store: &'static str,
    operation: &'static str,
    encoded: &str,
) -> Result<String, StoreError> {
    let hex = encoded
        .strip_prefix('i')
        .ok_or_else(|| StoreError::store(store, operation, "encoded id must start with 'i'"))?;
    if hex.len() % 2 != 0 {
        return Err(StoreError::store(
            store,
            operation,
            "encoded id has odd length",
        ));
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::store(store, operation, error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| StoreError::store(store, operation, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_path_unsafe_and_non_ascii_ids() {
        for id in ["plain", "slashes/and spaces", "oxide-🦀", ""] {
            assert_eq!(decode_id("test", "decode", &encode_id(id)).unwrap(), id);
        }
    }
}
