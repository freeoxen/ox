//! Re-export the upstream StructFS store combinator.

pub use structfs_core_store::Cascade;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalConfig;
    use structfs_core_store::{Reader, Record, Writer};
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
        let (_, mut fallback) = cascade.into_inner();
        let record = fallback.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("original".into())
        );
    }
}
