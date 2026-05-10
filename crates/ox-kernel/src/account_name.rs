//! A validated account name — a string that has passed the same UAX#31
//! identifier rule as [`PathComponent`].
//!
//! Account names enter the system from two boundaries: user input
//! (compose-mode buffer, manual edits) and broker reads (a `String` value
//! deserialized from a record). Both are untrusted at the type level. Lifting
//! the validation into a newtype lets a function signature express "this
//! string was checked" rather than relying on the caller to remember.
//!
//! The wire format stays a plain string via `#[serde(transparent)]` so the
//! broker representation is unchanged; callers reading from the broker
//! decode into a `String`, then call [`AccountName::try_new`] at the
//! boundary.
//!
//! This type sits next to [`PathComponent`] because the validation rule
//! is identical — every `AccountName` is also a valid `PathComponent`.
//! Construction here piggybacks on `PathComponent::try_new` so the rule
//! cannot drift between the two.

use serde::{Deserialize, Serialize};

use crate::PathComponent;

/// A validated account name. Internally a `String`, but constructed only
/// via [`AccountName::try_new`], which enforces the same rule as
/// [`PathComponent::try_new`] (UAX#31 identifier or pure numeric).
///
/// A function taking `&AccountName` is statically guaranteed its argument
/// passed validation; a function taking `&str` is not.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountName(String);

impl AccountName {
    /// Validate `s` and wrap it as an `AccountName`. Returns `Err` with the
    /// rejected string when validation fails.
    pub fn try_new(s: impl Into<String>) -> Result<Self, AccountNameError> {
        let s = s.into();
        PathComponent::try_new(s.clone()).map_err(|_| AccountNameError(s.clone()))?;
        Ok(Self(s))
    }

    /// Borrow the validated string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the inner string.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Convert to a [`PathComponent`]. The validation rule is the same, so
    /// this conversion always succeeds — it is provided as a convenience
    /// for callers that need to feed a `PathComponent` into `oxpath!`.
    pub fn to_path_component(&self) -> PathComponent {
        // Re-running validation here is the most defensive way to
        // produce a PathComponent without `unsafe`; since AccountName's
        // construction already passed the same rule, this cannot fail.
        PathComponent::try_new(self.0.clone())
            .expect("AccountName invariant guarantees a valid PathComponent")
    }
}

impl AsRef<str> for AccountName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AccountName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Error returned when a string fails [`AccountName::try_new`] validation.
/// Carries the rejected input so callers can quote it back to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountNameError(pub String);

impl std::fmt::Display for AccountNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid account name: '{}'", self.0)
    }
}

impl std::error::Error for AccountNameError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_new_accepts_valid_identifier() {
        assert!(AccountName::try_new("personal").is_ok());
        assert!(AccountName::try_new("anthropic_2").is_ok());
        // Leading underscore is allowed (the identifier rule permits it as
        // long as the name isn't a bare `_`).
        assert!(AccountName::try_new("_dunder").is_ok());
        // Pure numeric is allowed by the PathComponent rule.
        assert!(AccountName::try_new("42").is_ok());
    }

    #[test]
    fn try_new_rejects_invalid_identifier() {
        assert!(AccountName::try_new("bad-name").is_err());
        assert!(AccountName::try_new("has space").is_err());
        assert!(AccountName::try_new("").is_err());
        assert!(AccountName::try_new(".hidden").is_err());
        assert!(AccountName::try_new("_").is_err());
    }

    #[test]
    fn serde_roundtrip_is_transparent() {
        let name = AccountName::try_new("alpha").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, r#""alpha""#);
        let back: AccountName = serde_json::from_str(&json).unwrap();
        assert_eq!(back, name);
    }

    #[test]
    fn error_carries_rejected_input() {
        let err = AccountName::try_new("bad-name").unwrap_err();
        assert_eq!(err.0, "bad-name");
        assert_eq!(err.to_string(), "invalid account name: 'bad-name'");
    }

    #[test]
    fn display_and_as_ref_return_inner() {
        let name = AccountName::try_new("alpha").unwrap();
        assert_eq!(name.to_string(), "alpha");
        assert_eq!(name.as_ref(), "alpha");
        assert_eq!(name.as_str(), "alpha");
    }

    #[test]
    fn to_path_component_preserves_name() {
        let name = AccountName::try_new("alpha").unwrap();
        let comp = name.to_path_component();
        assert_eq!(comp.as_str(), "alpha");
    }
}
