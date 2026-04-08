//! ReadOnly — capability restriction wrapper that rejects all writes.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};

/// Wraps any Reader, rejecting all writes.
///
/// Use this to give stores read-only access to a config source.
pub struct ReadOnly<S> {
    inner: S,
}

impl<S> ReadOnly<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    /// Consume the wrapper and return the inner store.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: Reader> Reader for ReadOnly<S> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.inner.read(from)
    }
}

impl<S: Send + Sync> Writer for ReadOnly<S> {
    fn write(&mut self, _to: &Path, _data: Record) -> Result<Path, StoreError> {
        Err(StoreError::store(
            "ReadOnly",
            "write",
            "this handle is read-only",
        ))
    }
}

// Send + Sync if inner is
unsafe impl<S: Send> Send for ReadOnly<S> {}
unsafe impl<S: Sync> Sync for ReadOnly<S> {}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Value, path};

    struct TestStore {
        value: Value,
    }

    impl Reader for TestStore {
        fn read(&mut self, _from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(Some(Record::parsed(self.value.clone())))
        }
    }

    impl Writer for TestStore {
        fn write(&mut self, to: &Path, _data: Record) -> Result<Path, StoreError> {
            Ok(to.clone())
        }
    }

    #[test]
    fn read_passes_through() {
        let inner = TestStore {
            value: Value::String("hello".into()),
        };
        let mut ro = ReadOnly::new(inner);
        let result = ro.read(&path!("anything")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("hello".into()));
    }

    #[test]
    fn write_rejected() {
        let inner = TestStore {
            value: Value::Null,
        };
        let mut ro = ReadOnly::new(inner);
        let result = ro.write(&path!("anything"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn into_inner_recovers_store() {
        let inner = TestStore {
            value: Value::Integer(42),
        };
        let ro = ReadOnly::new(inner);
        let mut recovered = ro.into_inner();
        let result = recovered.read(&path!("x")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::Integer(42));
    }
}
