//! `AccountDeleteCleanupSubscription` — fires on null writes at
//! `config/gate/accounts/{name}` (account-record depth).
//!
//! Reactive observer of account deletion. The CLI's
//! `accounts.confirm.delete` command writes `Null` to the canonical
//! account path; this subscription watches the broader
//! `Prefix(config/gate/accounts)` pattern and filters at the top of
//! `handle` for null writes at account-record depth (one component
//! below the prefix).
//!
//! Cleanup body fans out the cross-cutting work the CLI shouldn't do
//! itself: deletes the API key, deletes the synthesized provider
//! record, and clears the `accounts/selected` pointer if it matched
//! the deleted account. The cursor is intentionally not touched —
//! the delete-confirm UI is an inline banner over `settings/index`,
//! so the user never left a renderable cursor.
//!
//! Returning all the writes from `handle` (rather than issuing them as
//! ad-hoc `writer.write` calls) lets the dispatcher cascade them as a
//! single logical event, so the snapshot built for the next render
//! sees the post-delete world atomically.

use ox_broker::subscription::{SubCtx, Subscription};
use ox_path::oxpath;
use ox_types::subscription::{PathPattern, SubscriptionId, Write};

use crate::subscriptions::util::{null_write, provider_path, read_typed_via_reader, secret_key_path};

pub const ID: &str = "gate.account_delete_cleanup";

pub struct AccountDeleteCleanupSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
}

impl Default for AccountDeleteCleanupSubscription {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountDeleteCleanupSubscription {
    pub fn new() -> Self {
        Self {
            id: SubscriptionId(ID.to_string()),
            watches: vec![PathPattern::Prefix(oxpath!("config", "gate", "accounts"))],
        }
    }
}

impl Subscription for AccountDeleteCleanupSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        use structfs_core_store::Value;

        let prefix = oxpath!("config", "gate", "accounts");

        // Filter 1: only react at account-record depth (prefix + 1 component).
        // Writes to children (`.../models`, `.../test_status`, etc.) get
        // skipped here.
        if ctx.change.path.len() != prefix.len() + 1 {
            return vec![];
        }

        // Filter 2: only react to deletes (Null writes). Updates and
        // creates fall through.
        let Some(record) = ctx.change.after.as_ref() else {
            return vec![];
        };
        if !matches!(record.as_value(), Some(Value::Null)) {
            return vec![];
        }

        // Extract the account name. The path's last component is the
        // account identifier; we already validated depth above.
        let name = ctx
            .change
            .path
            .components
            .last()
            .cloned()
            .unwrap_or_default();
        if name.is_empty() {
            return vec![];
        }

        // Side-data cleanup. The account record itself is already gone
        // (the user's null-write triggered us); we don't repeat that.
        let mut writes: Vec<Write> = Vec::new();

        // Delete the API key.
        if let Ok(p) = secret_key_path(&name) {
            writes.push(null_write(p));
        }
        // Delete the synthesized provider entry. v1 has one provider
        // per account named after the account; nothing to do for users
        // who hand-edit shared providers.
        if let Ok(p) = provider_path(&name) {
            writes.push(null_write(p));
        }

        // Clear selection if it pointed at the deleted account.
        let selected_path = oxpath!("ui", "settings", "accounts", "selected");
        let selected: Option<String> = read_typed_via_reader(ctx.snapshot, &selected_path);
        if selected.as_deref() == Some(name.as_str()) {
            // Null write deletes the selection. The renderer treats a
            // missing record the same as `None`.
            writes.push(null_write(selected_path));
        }

        writes
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ox_broker::subscription::{AsyncWriter, SubCtx, Subscription};
    use ox_path::oxpath;
    use ox_types::subscription::PathChange;
    use structfs_core_store::{Path, Record, Value};

    use super::*;
    use crate::subscriptions::util::testing::{
        CapturingWriter, InMemoryReader, TestSpawn, populate_anthropic_account,
    };

    fn trigger_path(name: &str) -> Path {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        // Canonical account path; the user's null-write here IS the delete.
        oxpath!("config", "gate", "accounts", comp)
    }

    fn drive(reader: &mut InMemoryReader, name: &str) -> Vec<Write> {
        let sub = AccountDeleteCleanupSubscription::new();
        let path = trigger_path(name);
        let change = PathChange {
            path,
            // The handler does not read `before`; any value is fine.
            before: Some(Record::parsed(Value::Null)),
            after: Some(Record::parsed(Value::Null)),
        };
        let spawn = TestSpawn::new();
        let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;
        let ctx = SubCtx {
            snapshot: reader,
            change: &change,
            spawn: &spawn,
            writer,
        };
        sub.handle(ctx)
    }

    fn paths(writes: &[Write]) -> Vec<String> {
        writes.iter().map(|w| w.path.to_string()).collect()
    }

    fn null_record(writes: &[Write], path_str: &str) -> bool {
        writes.iter().any(|w| {
            w.path.to_string() == path_str && matches!(w.record.as_value(), Some(Value::Null))
        })
    }

    #[test]
    fn cleanup_removes_key_and_provider() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        let writes = drive(&mut reader, "alpha");
        // The account record itself is deleted by the user's write;
        // the cleanup body must not also write at that path.
        assert!(
            !null_record(&writes, "config/gate/accounts/alpha"),
            "cleanup must not redo the account-record null-write; got {:?}",
            paths(&writes)
        );
        assert!(
            null_record(&writes, "secret/keys/alpha"),
            "{:?}",
            paths(&writes)
        );
        assert!(
            null_record(&writes, "config/gate/providers/alpha"),
            "{:?}",
            paths(&writes)
        );
    }

    #[test]
    fn cleanup_clears_selection_only_when_matching() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");
        reader.set("ui/settings/accounts/selected", &"alpha".to_string());

        let writes = drive(&mut reader, "alpha");
        // The selected key gets a None write — i.e. JSON null in the
        // serde representation; structfs-serde-store maps Option::None
        // to Value::Null.
        let cleared = writes.iter().any(|w| {
            w.path.to_string() == "ui/settings/accounts/selected"
                && matches!(w.record.as_value(), Some(Value::Null))
        });
        assert!(
            cleared,
            "selection should be cleared, got {:?}",
            paths(&writes)
        );
    }

    #[test]
    fn cleanup_does_not_clear_selection_pointing_at_other_account() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");
        populate_anthropic_account(&mut reader, "beta", "sk-key2");
        reader.set("ui/settings/accounts/selected", &"beta".to_string());

        let writes = drive(&mut reader, "alpha");
        let touched_selection = writes
            .iter()
            .any(|w| w.path.to_string() == "ui/settings/accounts/selected");
        assert!(
            !touched_selection,
            "selection pointing at a different account must be left alone, got {:?}",
            paths(&writes)
        );
    }

    #[test]
    fn cleanup_does_not_touch_cursor() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-test");
        let writes = drive(&mut reader, "alpha");
        assert!(
            !writes
                .iter()
                .any(|w| w.path == oxpath!("ui", "settings", "cursor")),
            "cleanup must not touch the cursor; got {writes:?}"
        );
    }

    #[test]
    fn cleanup_skips_writes_to_child_paths() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-test");
        // Drive a write to a child path (e.g. .../models). The subscription
        // must not fire its cleanup body — that path isn't an account
        // record, even though it matches the Prefix watch.
        let sub = AccountDeleteCleanupSubscription::new();
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        let path = oxpath!("config", "gate", "accounts", comp, "models");
        let change = PathChange {
            path,
            before: None,
            after: Some(Record::parsed(Value::Null)),
        };
        let spawn = TestSpawn::new();
        let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;
        let ctx = SubCtx {
            snapshot: &mut reader,
            change: &change,
            spawn: &spawn,
            writer,
        };
        let writes = sub.handle(ctx);
        assert!(
            writes.is_empty(),
            "child-path writes must not trigger cleanup; got {writes:?}"
        );
    }

    #[test]
    fn cleanup_skips_non_null_writes_at_account_depth() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-test");
        // An update (non-null write) at the account-record path should not
        // trigger the cleanup body — the account isn't being deleted.
        let sub = AccountDeleteCleanupSubscription::new();
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        let path = oxpath!("config", "gate", "accounts", comp);
        let cfg = crate::AccountConfig {
            provider: "anthropic".into(),
        };
        let change = PathChange {
            path,
            before: None,
            after: Some(Record::parsed(structfs_serde_store::to_value(&cfg).unwrap())),
        };
        let spawn = TestSpawn::new();
        let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;
        let ctx = SubCtx {
            snapshot: &mut reader,
            change: &change,
            spawn: &spawn,
            writer,
        };
        let writes = sub.handle(ctx);
        assert!(
            writes.is_empty(),
            "non-null writes at account depth must not trigger cleanup; got {writes:?}"
        );
    }
}
