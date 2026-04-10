use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};

/// The decision returned by a policy check for a given write.
pub enum PolicyDecision {
    Allow,
    Deny(String),
}

/// Trait for checking whether a tool write should proceed.
///
/// Implementations may block for user approval (the Ask flow) — this is
/// intentional. The CLI implementation writes an approval request to the
/// broker and blocks until the TUI responds.
pub trait PolicyCheck: Send + Sync {
    /// Check whether a write to `path` with `data` should be allowed.
    /// May block for user approval.
    fn check(&mut self, path: &Path, data: &Record) -> PolicyDecision;
}

/// A generic store wrapper that intercepts writes for policy enforcement.
///
/// Reads always pass through to the inner store unchanged. Writes are checked
/// against the [`PolicyCheck`] implementation before being forwarded; a `Deny`
/// decision returns a `StoreError` without touching the inner store.
pub struct PolicyStore<S: Reader + Writer, P: PolicyCheck> {
    pub inner: S,
    policy: P,
}

impl<S: Reader + Writer, P: PolicyCheck> PolicyStore<S, P> {
    pub fn new(inner: S, policy: P) -> Self {
        Self { inner, policy }
    }
}

impl<S: Reader + Writer, P: PolicyCheck> Reader for PolicyStore<S, P> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.inner.read(from)
    }
}

impl<S: Reader + Writer, P: PolicyCheck> Writer for PolicyStore<S, P> {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        match self.policy.check(to, &data) {
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

    struct AllowAll;
    impl PolicyCheck for AllowAll {
        fn check(&mut self, _path: &Path, _data: &Record) -> PolicyDecision {
            PolicyDecision::Allow
        }
    }

    struct DenyAll(String);
    impl PolicyCheck for DenyAll {
        fn check(&mut self, _path: &Path, _data: &Record) -> PolicyDecision {
            PolicyDecision::Deny(self.0.clone())
        }
    }

    #[test]
    fn allow_policy_passes_through() {
        let mock = MockStore::new();
        let mut store = PolicyStore::new(mock, AllowAll);

        let path = Path::parse("some/path").unwrap();
        let record = Record::parsed(Value::String("data".to_string()));
        let result = store.write(&path, record);

        assert!(result.is_ok());
        assert_eq!(store.inner.writes.len(), 1);
    }

    #[test]
    fn deny_policy_blocks_write() {
        let mock = MockStore::new();
        let mut store = PolicyStore::new(mock, DenyAll("not allowed".into()));

        let path = Path::parse("some/path").unwrap();
        let record = Record::parsed(Value::String("data".to_string()));
        let result = store.write(&path, record);

        assert!(result.is_err());
        assert_eq!(store.inner.writes.len(), 0);
    }

    #[test]
    fn reads_pass_through_ungated() {
        let mock = MockStore::new();
        let mut store = PolicyStore::new(mock, DenyAll("deny everything".into()));

        let path = Path::parse("some/path").unwrap();
        let result = store.read(&path);

        assert!(result.is_ok());
        let record = result.unwrap();
        assert!(record.is_some());
    }
}
