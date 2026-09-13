//! A validated path component — can only be constructed from a string that
//! passes UAX#31 validation (or is pure numeric).
//!
//! Use [`PathComponent::try_new`] for runtime validation, or the [`path!`]
//! macro for compile-time validated literals.

use crate::{Path, StoreError};

/// A single validated path component.
///
/// Guarantees: the inner string is a valid StructFS path component (UAX#31
/// identifier or pure numeric). Cannot be constructed from an arbitrary string
/// without validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathComponent(String);

impl PathComponent {
    /// Validate and wrap a string as a path component.
    ///
    /// Returns an error if the string is not a valid UAX#31 identifier
    /// or pure numeric string.
    pub fn try_new(s: impl Into<String>) -> Result<Self, StoreError> {
        let s = s.into();
        // Use Path::try_from_components as the validation oracle — it applies
        // the same UAX#31 rules that structfs uses internally.
        Path::try_from_components(vec![s.clone()])
            .map_err(|e| StoreError::store("PathComponent", "try_new", e.to_string()))?;
        Ok(Self(s))
    }

    /// Get the validated string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Borrow the validated string — used by the `path!` macro to enforce
    /// that only `PathComponent` values (not bare `String`/`&str`) are accepted
    /// as runtime path components. Named distinctly so no standard type matches.
    pub fn validated_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the inner string.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for PathComponent {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PathComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ascii_identifier() {
        let c = PathComponent::try_new("accounts").unwrap();
        assert_eq!(c.as_str(), "accounts");
    }

    #[test]
    fn valid_numeric() {
        let c = PathComponent::try_new("42").unwrap();
        assert_eq!(c.as_str(), "42");
    }

    #[test]
    fn valid_unicode_identifier() {
        let c = PathComponent::try_new("café").unwrap();
        assert_eq!(c.as_str(), "café");
    }

    #[test]
    fn valid_underscore_prefix() {
        let c = PathComponent::try_new("_private").unwrap();
        assert_eq!(c.as_str(), "_private");
    }

    #[test]
    fn rejects_empty() {
        assert!(PathComponent::try_new("").is_err());
    }

    #[test]
    fn rejects_hyphen() {
        assert!(PathComponent::try_new("my-account").is_err());
    }

    #[test]
    fn rejects_space() {
        assert!(PathComponent::try_new("my account").is_err());
    }

    #[test]
    fn rejects_dot_prefix() {
        assert!(PathComponent::try_new(".hidden").is_err());
    }

    #[test]
    fn rejects_bare_underscore() {
        assert!(PathComponent::try_new("_").is_err());
    }

    // -- path! macro tests --

    #[test]
    fn path_all_literals() {
        let p = crate::path!("gate", "defaults", "model");
        assert_eq!(p.to_string(), "gate/defaults/model");
    }

    #[test]
    fn path_single_literal() {
        let p = crate::path!("system");
        assert_eq!(p.to_string(), "system");
    }

    #[test]
    fn path_with_runtime_component() {
        let name = PathComponent::try_new("personal").unwrap();
        let p = crate::path!("gate", "accounts", name, "provider");
        assert_eq!(p.to_string(), "gate/accounts/personal/provider");
        assert_eq!(
            crate::path!("gate/accounts", name).to_string(),
            "gate/accounts/personal"
        );
    }

    #[test]
    fn path_numeric_literal() {
        let p = crate::path!("items", 0, "name");
        assert_eq!(p.to_string(), "items/0/name");
    }

    #[test]
    fn path_unicode_literal() {
        let p = crate::path!("données", "utilisateur");
        assert_eq!(p.to_string(), "données/utilisateur");
    }

    #[test]
    fn path_empty() {
        let p = crate::path!();
        assert!(p.is_empty());
        assert!(crate::path!("").is_empty());
    }
}
