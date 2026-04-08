//! Command construction helper for broker writes.
//!
//! The `cmd!` macro builds a `Record::parsed(Value::Map(...))` in one line,
//! eliminating the 3-5 line BTreeMap boilerplate at every broker write site.

use structfs_core_store::Value;

/// Trait for converting Rust values into StructFS Values.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::String(self.to_string())
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::String(self)
    }
}

impl IntoValue for &String {
    fn into_value(self) -> Value {
        Value::String(self.clone())
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Value {
        Value::Integer(self)
    }
}

impl IntoValue for usize {
    fn into_value(self) -> Value {
        Value::Integer(self as i64)
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

/// Helper function used by the cmd! macro.
pub fn into_value(v: impl IntoValue) -> Value {
    v.into_value()
}

/// Build a broker command Record from key-value pairs.
///
/// Returns `Record::parsed(Value::Map(...))`.
///
/// Usage:
/// - `cmd!()` — empty command
/// - `cmd!("key" => "value")` — single field
/// - `cmd!("a" => 1_i64, "b" => "two")` — multiple fields
#[macro_export]
macro_rules! cmd {
    () => {
        structfs_core_store::Record::parsed(
            structfs_core_store::Value::Map(std::collections::BTreeMap::new()),
        )
    };
    ($($key:expr => $val:expr),+ $(,)?) => {{
        let mut map = std::collections::BTreeMap::new();
        $(map.insert($key.to_string(), $crate::broker_cmd::into_value($val));)+
        structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map))
    }};
}

#[cfg(test)]
mod tests {
    use structfs_core_store::Value;

    #[test]
    fn cmd_empty() {
        let record = cmd!();
        let val = record.as_value().unwrap();
        assert!(matches!(val, Value::Map(m) if m.is_empty()));
    }

    #[test]
    fn cmd_single_string() {
        let record = cmd!("name" => "alice");
        let val = record.as_value().unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("name"), Some(&Value::String("alice".into())));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn cmd_multiple_types() {
        let record = cmd!("text" => "hello", "cursor" => 5_usize, "flag" => true);
        let val = record.as_value().unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("text"), Some(&Value::String("hello".into())));
                assert_eq!(m.get("cursor"), Some(&Value::Integer(5)));
                assert_eq!(m.get("flag"), Some(&Value::Bool(true)));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn cmd_owned_string() {
        let s = String::from("owned");
        let record = cmd!("key" => s);
        let val = record.as_value().unwrap();
        match val {
            Value::Map(m) => assert_eq!(m.get("key"), Some(&Value::String("owned".into()))),
            _ => panic!("expected Map"),
        }
    }
}
