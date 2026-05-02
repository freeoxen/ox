//! `AccountCreateSubscription` — fires on writes to the exact path
//! `config/gate/accounts/_create_now`.
//!
//! The CLI's `accounts.create` command writes a `CreateAccountRequest`
//! at this path; this subscription validates the name as a path
//! component, materializes a default `AccountConfig`, selects the new
//! account, and pops the cursor to the detail page.
//!
//! On invalid name we don't allocate anything — instead we emit a
//! transient error banner so the user sees what went wrong.
//!
//! Per spec §6.4 / §6.5 (overlay action wires).
//!
//! ## Note: `CreateAccountRequest` lives in `ox-types::settings`
//!
//! Phase L3 originally placed `CreateAccountRequest` in the CLI command
//! module. N6 moves it to `ox_types::settings::CreateAccountRequest` so
//! the broker subscription, the CLI commands, and any future renderer
//! can all agree on the shape without an ox-cli dep.

use ox_broker::subscription::{SubCtx, Subscription};
use ox_kernel::PathComponent;
use ox_path::oxpath;
use ox_types::settings::{CreateAccountRequest, GlobalBanner};
use ox_types::subscription::{PathPattern, SubscriptionId, Write};
use structfs_core_store::Record;

use crate::AccountConfig;
use crate::subscriptions::util::{account_path, now_ms, null_write, write_path, write_typed};

pub const ID: &str = "gate.account_create";

/// Default provider id assigned to newly-created accounts. The CLI's
/// new-account overlay lets the user pick a preset before submitting,
/// but this subscription synthesizes a default for the case where the
/// overlay was bypassed (programmatic create).
const DEFAULT_PROVIDER: &str = "anthropic";

pub struct AccountCreateSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
}

impl Default for AccountCreateSubscription {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountCreateSubscription {
    pub fn new() -> Self {
        Self {
            id: SubscriptionId(ID.to_string()),
            watches: vec![PathPattern::Exact(oxpath!(
                "config",
                "gate",
                "accounts",
                "_create_now"
            ))],
        }
    }
}

impl Subscription for AccountCreateSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        // The change.after carries the parsed `CreateAccountRequest`.
        // Missing or malformed → no-op.
        let Some(record) = ctx.change.after.as_ref() else {
            return vec![];
        };
        let Some(value) = record.as_value() else {
            return vec![];
        };
        let req: CreateAccountRequest = match structfs_serde_store::from_value(value.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "account_create: malformed request payload");
                return vec![];
            }
        };

        // Validate the name is a real path component.
        if PathComponent::try_new(req.name.clone()).is_err() {
            tracing::warn!(name = %req.name, "account_create: invalid name");
            return vec![banner_error(format!(
                "Invalid account name: '{}'",
                req.name
            ))];
        }

        let Ok(acct_path) = account_path(&req.name) else {
            // Already validated above; this branch is defensive.
            return vec![banner_error(format!(
                "Invalid account name: '{}'",
                req.name
            ))];
        };

        let cfg = AccountConfig {
            provider: DEFAULT_PROVIDER.to_string(),
        };

        // 1. Materialize the default config.
        // 2. Select the new account.
        // 3. Drive the cursor to the detail page.
        // 4. Clear `_create_now` so the same name can be created again
        //    in a session — keeps the path tidy and avoids dangling
        //    request payloads.
        vec![
            write_typed(&acct_path, &cfg),
            write_typed(
                &oxpath!("ui", "settings", "accounts", "selected"),
                &Some(req.name.clone()),
            ),
            write_path(
                &oxpath!("ui", "settings", "cursor"),
                &oxpath!("settings", "accounts", "_detail"),
            ),
            null_write(oxpath!("config", "gate", "accounts", "_create_now")),
        ]
    }
}

fn banner_error(message: String) -> Write {
    let banner = GlobalBanner::Error {
        message,
        set_at_ms: now_ms(),
    };
    Write {
        path: oxpath!("ui", "global", "banner"),
        record: Record::parsed(structfs_serde_store::to_value(&banner).unwrap()),
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

    fn drive(reader: &mut InMemoryReader, req: &CreateAccountRequest) -> Vec<Write> {
        let sub = AccountCreateSubscription::new();
        let trigger = oxpath!("config", "gate", "accounts", "_create_now");
        let after_value = structfs_serde_store::to_value(req).unwrap();
        let change = PathChange {
            path: trigger,
            before: None,
            after: Some(Record::parsed(after_value)),
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

    #[test]
    fn create_writes_default_config_selection_cursor() {
        let mut reader = InMemoryReader::new();
        let writes = drive(
            &mut reader,
            &CreateAccountRequest {
                name: "alpha".into(),
            },
        );

        // Account record at the canonical path.
        let acct_write = writes
            .iter()
            .find(|w| w.path.to_string() == "config/gate/accounts/alpha")
            .expect("missing account write");
        let cfg: AccountConfig =
            structfs_serde_store::from_value(acct_write.record.as_value().unwrap().clone())
                .unwrap();
        assert_eq!(cfg.provider, DEFAULT_PROVIDER);

        // Selection.
        let sel_write = writes
            .iter()
            .find(|w| w.path.to_string() == "ui/settings/accounts/selected")
            .expect("missing selection write");
        let sel: Option<String> =
            structfs_serde_store::from_value(sel_write.record.as_value().unwrap().clone()).unwrap();
        assert_eq!(sel.as_deref(), Some("alpha"));

        // Cursor → settings/accounts/_detail.
        let cur_write = writes
            .iter()
            .find(|w| w.path.to_string() == "ui/settings/cursor")
            .expect("missing cursor write");
        match cur_write.record.as_value() {
            Some(Value::Array(segs)) => {
                let parts: Vec<String> = segs
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        _ => panic!("non-string segment"),
                    })
                    .collect();
                assert_eq!(parts.join("/"), "settings/accounts/_detail");
            }
            other => panic!("cursor must be Value::Array, got {other:?}"),
        }

        // _create_now cleared.
        let null = writes.iter().any(|w| {
            w.path.to_string() == "config/gate/accounts/_create_now"
                && matches!(w.record.as_value(), Some(Value::Null))
        });
        assert!(null, "create_now must be cleared after handling");
    }

    #[test]
    fn create_rejects_invalid_name_with_banner() {
        let mut reader = InMemoryReader::new();
        // Hyphenated name fails PathComponent::try_new.
        let writes = drive(
            &mut reader,
            &CreateAccountRequest {
                name: "bad-name".into(),
            },
        );
        // Only one write — the banner — and no account record.
        let banner_write = writes
            .iter()
            .find(|w| w.path.to_string() == "ui/global/banner")
            .expect("banner missing");
        let banner: GlobalBanner =
            structfs_serde_store::from_value(banner_write.record.as_value().unwrap().clone())
                .unwrap();
        match banner {
            GlobalBanner::Error { message, .. } => {
                assert!(message.contains("bad-name"), "got: {message}");
            }
            other => panic!("expected Error banner, got {other:?}"),
        }
        assert!(
            !writes
                .iter()
                .any(|w| w.path.to_string() == "config/gate/accounts/bad-name")
        );
    }

    #[test]
    fn create_inert_when_after_record_is_missing() {
        // change.after = None. The handler should write nothing.
        let sub = AccountCreateSubscription::new();
        let trigger = oxpath!("config", "gate", "accounts", "_create_now");
        let change = PathChange {
            path: trigger,
            before: None,
            after: None,
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
