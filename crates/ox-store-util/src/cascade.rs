//! Cascade<A, B> — layered read with fallback.
//!
//! Reads try A first; if A returns None, falls back to B.
//! Writes go to A (the overlay). B is read-only from Cascade's perspective.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};

/// Layered store: reads try `primary` first, fall back to `fallback`.
/// Writes always go to `primary`.
pub struct Cascade<A, B> {
    pub primary: A,
    pub fallback: B,
}

impl<A, B> Cascade<A, B> {
    pub fn new(primary: A, fallback: B) -> Self {
        Self { primary, fallback }
    }
}

impl<A: Reader, B: Reader> Reader for Cascade<A, B> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        match self.primary.read(from)? {
            Some(record) => Ok(Some(record)),
            None => self.fallback.read(from),
        }
    }
}

impl<A: Writer, B: Send + Sync> Writer for Cascade<A, B> {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        self.primary.write(to, data)
    }
}

unsafe impl<A: Send, B: Send> Send for Cascade<A, B> {}
unsafe impl<A: Sync, B: Sync> Sync for Cascade<A, B> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalConfig;
    use structfs_core_store::{Value, path};

    #[test]
    fn primary_value_wins() {
        let mut primary = LocalConfig::new();
        primary.set("gate/model", Value::String("primary-model".into()));
        let mut fallback = LocalConfig::new();
        fallback.set("gate/model", Value::String("fallback-model".into()));
        let mut cascade = Cascade::new(primary, fallback);
        let record = cascade.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("primary-model".into())
        );
    }

    #[test]
    fn falls_back_when_primary_returns_none() {
        let primary = LocalConfig::new();
        let mut fallback = LocalConfig::new();
        fallback.set("gate/model", Value::String("fallback-model".into()));
        let mut cascade = Cascade::new(primary, fallback);
        let record = cascade.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("fallback-model".into())
        );
    }

    #[test]
    fn both_none_returns_none() {
        let mut cascade = Cascade::new(LocalConfig::new(), LocalConfig::new());
        assert!(cascade.read(&path!("gate/model")).unwrap().is_none());
    }

    #[test]
    fn writes_go_to_primary() {
        let mut fallback = LocalConfig::new();
        fallback.set("gate/model", Value::String("fallback".into()));
        let mut cascade = Cascade::new(LocalConfig::new(), fallback);
        cascade
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("written".into())),
            )
            .unwrap();
        let record = cascade.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(record.as_value().unwrap(), &Value::String("written".into()));
    }

    #[test]
    fn write_to_primary_does_not_affect_fallback() {
        let mut fallback = LocalConfig::new();
        fallback.set("gate/model", Value::String("original".into()));
        let mut cascade = Cascade::new(LocalConfig::new(), fallback);
        cascade
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("override".into())),
            )
            .unwrap();
        let record = cascade
            .fallback
            .read(&path!("gate/model"))
            .unwrap()
            .unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("original".into())
        );
    }
}
