//! `AccountDeleteSubscription` — fires on writes to
//! `config/gate/accounts/{name}/delete_now`.
//!
//! Fully synchronous: there is no network call to make, just a fan-out
//! of writes that delete the account's record, its API key, and its
//! synthesized provider entry; clear the selection if it pointed at the
//! deleted account; and pop the cursor back to the accounts list.
//!
//! Returning all the writes from `handle` (rather than issuing them as
//! ad-hoc `writer.write` calls) lets the dispatcher cascade them as a
//! single logical event, so the snapshot built for the next render
//! sees the post-delete world atomically.
//!
//! Per spec §6.5.

use ox_broker::subscription::{SubCtx, Subscription};
use ox_path::oxpath;
use ox_types::subscription::{PathPattern, SubscriptionId, Write};

use crate::AccountConfig;
use crate::subscriptions::util::{
    account_path, instance_segment, null_write, provider_path, read_typed_via_reader,
    secret_key_path, write_path,
};

pub const ID: &str = "gate.account_delete";

pub struct AccountDeleteSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
}

impl Default for AccountDeleteSubscription {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountDeleteSubscription {
    pub fn new() -> Self {
        Self {
            id: SubscriptionId(ID.to_string()),
            watches: vec![PathPattern::PrefixSuffix {
                prefix: oxpath!("config", "gate", "accounts"),
                suffix: oxpath!("delete_now"),
            }],
        }
    }
}

impl Subscription for AccountDeleteSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        let prefix = oxpath!("config", "gate", "accounts");
        let suffix = oxpath!("delete_now");
        let Some(name) = instance_segment(&ctx.change.path, &prefix, &suffix) else {
            return vec![];
        };

        // No-op when the account doesn't exist: idempotent delete.
        let Ok(acct_path) = account_path(&name) else {
            return vec![];
        };
        let exists: Option<AccountConfig> = read_typed_via_reader(ctx.snapshot, &acct_path);
        if exists.is_none() {
            tracing::debug!(account = %name, "account_delete: no record at path; nothing to do");
            return vec![];
        }

        let mut writes: Vec<Write> = Vec::new();
        // Delete the account record.
        writes.push(null_write(acct_path));
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

        // Cursor back to the accounts list.
        let cursor_path = oxpath!("ui", "settings", "cursor");
        let new_cursor = oxpath!("settings", "accounts");
        writes.push(write_path(&cursor_path, &new_cursor));

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
        oxpath!("config", "gate", "accounts", comp, "delete_now")
    }

    fn drive(reader: &mut InMemoryReader, name: &str) -> Vec<Write> {
        let sub = AccountDeleteSubscription::new();
        let path = trigger_path(name);
        let change = PathChange {
            path,
            before: None,
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
    fn delete_removes_account_record_key_and_provider() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        let writes = drive(&mut reader, "alpha");
        assert!(
            null_record(&writes, "config/gate/accounts/alpha"),
            "{:?}",
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
    fn delete_clears_selection_only_when_matching() {
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
    fn delete_does_not_clear_selection_pointing_at_other_account() {
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
    fn delete_pops_cursor_back_to_accounts_list() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        let writes = drive(&mut reader, "alpha");
        let cursor_write = writes
            .iter()
            .find(|w| w.path.to_string() == "ui/settings/cursor")
            .expect("cursor write missing");
        // Cursors are encoded as Value::Array of segment strings — the
        // shape `path_to_value` produces, mirroring the CLI commands.
        match cursor_write.record.as_value() {
            Some(Value::Array(segs)) => {
                let parts: Vec<String> = segs
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        _ => panic!("non-string segment: {v:?}"),
                    })
                    .collect();
                assert_eq!(parts.join("/"), "settings/accounts");
            }
            other => panic!("cursor must be Value::Array, got {other:?}"),
        }
    }

    #[test]
    fn delete_is_noop_when_account_missing() {
        let mut reader = InMemoryReader::new();
        // No accounts populated.
        let writes = drive(&mut reader, "ghost");
        assert!(writes.is_empty(), "got: {:?}", paths(&writes));
    }
}
