use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};

/// The decision returned by a policy function for a given write.
pub enum PolicyDecision {
    Allow,
    Deny(String),
    // Ask variant will be added when integrating with TUI approval flow.
}

/// A generic store wrapper that intercepts writes for policy enforcement.
///
/// Reads always pass through to the inner store unchanged. Writes are checked
/// against the policy function before being forwarded; a `Deny` decision
/// returns a `StoreError` without touching the inner store.
pub struct PolicyStore<S, F>
where
    S: Reader + Writer,
    F: FnMut(&Path, &Record) -> PolicyDecision + Send + Sync,
{
    pub inner: S,
    policy: F,
}

impl<S, F> PolicyStore<S, F>
where
    S: Reader + Writer,
    F: FnMut(&Path, &Record) -> PolicyDecision + Send + Sync,
{
    pub fn new(inner: S, policy: F) -> Self {
        Self { inner, policy }
    }
}

impl<S, F> Reader for PolicyStore<S, F>
where
    S: Reader + Writer,
    F: FnMut(&Path, &Record) -> PolicyDecision + Send + Sync,
{
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.inner.read(from)
    }
}

impl<S, F> Writer for PolicyStore<S, F>
where
    S: Reader + Writer,
    F: FnMut(&Path, &Record) -> PolicyDecision + Send + Sync,
{
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        match (self.policy)(to, &data) {
            PolicyDecision::Allow => self.inner.write(to, data),
            PolicyDecision::Deny(reason) => Err(StoreError::store("policy", "write", reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::Value;

    struct MockStore {
        writes: Vec<String>,
    }

    impl MockStore {
        fn new() -> Self {
            Self { writes: vec![] }
        }
    }

    impl Reader for MockStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(Some(Record::parsed(Value::String(format!("read:{from}")))))
        }
    }

    impl Writer for MockStore {
        fn write(&mut self, to: &Path, _data: Record) -> Result<Path, StoreError> {
            self.writes.push(to.to_string());
            Ok(to.clone())
        }
    }

    #[test]
    fn allow_policy_passes_through() {
        let mock = MockStore::new();
        let mut store = PolicyStore::new(mock, |_path, _record| PolicyDecision::Allow);

        let path = Path::parse("some/path").unwrap();
        let record = Record::parsed(Value::String("data".to_string()));
        let result = store.write(&path, record);

        assert!(result.is_ok());
        assert_eq!(store.inner.writes.len(), 1);
    }

    #[test]
    fn deny_policy_blocks_write() {
        let mock = MockStore::new();
        let mut store = PolicyStore::new(mock, |_path, _record| {
            PolicyDecision::Deny("not allowed".to_string())
        });

        let path = Path::parse("some/path").unwrap();
        let record = Record::parsed(Value::String("data".to_string()));
        let result = store.write(&path, record);

        assert!(result.is_err());
        assert_eq!(store.inner.writes.len(), 0);
    }

    #[test]
    fn reads_pass_through_ungated() {
        let mock = MockStore::new();
        let mut store = PolicyStore::new(mock, |_path, _record| {
            PolicyDecision::Deny("deny everything".to_string())
        });

        let path = Path::parse("some/path").unwrap();
        let result = store.read(&path);

        assert!(result.is_ok());
        let record = result.unwrap();
        assert!(record.is_some());
    }
}
