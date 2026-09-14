//! Validated components shared with StructFS and its path macro.

pub use structfs_core_store::PathComponent;

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
