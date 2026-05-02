//! `ConfigSaveSubscription` — fires on writes to the exact path
//! `config/save`.
//!
//! ## Why this is a no-op handler
//!
//! In Phase A's design (see `crates/ox-ui/src/config_store.rs:118-127`)
//! the `ConfigStore::write` impl already special-cases `key == "save"`
//! and triggers `save_runtime`, which persists the runtime overlay
//! through the store's `StoreBacking` (TomlFileBacking for config,
//! JsonFileBacking for secrets). The save is observable to the
//! triggering write — `save_runtime`'s error propagates back through
//! the ConfigStore Writer impl into the broker's write reply, and the
//! caller sees it.
//!
//! This subscription exists for **protocol uniformity** — the
//! settings-screen design asserts that every action path has a named
//! subscription handling it (per spec §3.3). Anyone reading
//! `register_all` should see "config_save is owned by ox-gate," not
//! "config_save is conjured by a special case in ox-ui." When the save
//! logic eventually moves out of ConfigStore (planned for Phase R),
//! this handler grows real bodies.
//!
//! Today it logs and returns `vec![]`.

use ox_broker::subscription::{SubCtx, Subscription};
use ox_path::oxpath;
use ox_types::subscription::{PathPattern, SubscriptionId, Write};

pub const ID: &str = "gate.config_save";

pub struct ConfigSaveSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
}

impl Default for ConfigSaveSubscription {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigSaveSubscription {
    pub fn new() -> Self {
        Self {
            id: SubscriptionId(ID.to_string()),
            watches: vec![PathPattern::Exact(oxpath!("config", "save"))],
        }
    }
}

impl Subscription for ConfigSaveSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, _ctx: SubCtx<'_>) -> Vec<Write> {
        // The actual save runs synchronously inside ConfigStore::write
        // when the path equals "save". This handler exists so the
        // protocol formally owns the trigger; future moves of the save
        // logic land here.
        tracing::info!("config save trigger observed");
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ox_broker::subscription::{AsyncWriter, SubCtx, Subscription};
    use ox_path::oxpath;
    use ox_types::subscription::PathChange;
    use structfs_core_store::{Record, Value};

    use super::*;
    use crate::subscriptions::util::testing::{CapturingWriter, InMemoryReader, TestSpawn};

    #[test]
    fn id_matches_const() {
        let sub = ConfigSaveSubscription::new();
        assert_eq!(sub.id().0, ID);
    }

    #[test]
    fn watches_config_save_exactly() {
        let sub = ConfigSaveSubscription::new();
        assert_eq!(sub.watches().len(), 1);
        match &sub.watches()[0] {
            PathPattern::Exact(p) => assert_eq!(p.to_string(), "config/save"),
            other => panic!("expected Exact, got {other:?}"),
        }
    }

    #[test]
    fn handle_returns_empty_writes() {
        let sub = ConfigSaveSubscription::new();
        let change = PathChange {
            path: oxpath!("config", "save"),
            before: None,
            after: Some(Record::parsed(Value::Null)),
        };
        let spawn = TestSpawn::new();
        let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;
        let mut reader = InMemoryReader::new();
        let ctx = SubCtx {
            snapshot: &mut reader,
            change: &change,
            spawn: &spawn,
            writer,
        };
        let writes = sub.handle(ctx);
        assert!(writes.is_empty());
    }
}
