//! Codec error type for inbound (wire → internal) translation.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CodecError {
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid shape: {0}")]
    InvalidShape(String),
    #[error("unsupported feature: {0}")]
    UnsupportedFeature(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_field_display() {
        let e = CodecError::MissingField("model");
        assert_eq!(e.to_string(), "missing required field: model");
    }

    #[test]
    fn invalid_shape_display() {
        let e = CodecError::InvalidShape("body must be a JSON object".into());
        assert_eq!(e.to_string(), "invalid shape: body must be a JSON object");
    }

    #[test]
    fn unsupported_feature_display() {
        let e = CodecError::UnsupportedFeature("streaming tools".into());
        assert_eq!(e.to_string(), "unsupported feature: streaming tools");
    }
}
