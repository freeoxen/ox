//! Active-shortcut registry for the settings screen.
//!
//! For the current focus cursor, this module maintains a single broker
//! record — a `Vec<KeyHint>` filtered to the bindings that are reachable
//! from the cursor's scope chain, deduped per key, and sorted by curated
//! priority. Consumers (the footer bar, the shortcuts modal, anything
//! later) read that one record; nobody re-projects per frame.
//!
//! The registry updates when the cursor moves, not on every render.
//! `ShortcutResolver` watches `cursor_path`; on fire, it re-projects
//! and writes the result to `shortcuts_path()`. Bindings + commands are
//! installed once at startup and never change at runtime, so the
//! resolver holds an in-memory snapshot of both — **no broker subtree
//! reads at runtime**. The only broker touch per fire is decoding the
//! new cursor out of `ctx.change.after`. This is load-bearing:
//! `LocalConfig::read` on a non-leaf path walks every key in the store
//! recursively, so even one re-read per cursor move at autorepeat speed
//! pegged CPU in the prior iteration.
//!
//! Ordering note: this subscription is registered *before* horns'
//! `RenderSubscription` so that when a cursor write fires both, the
//! resolver writes the fresh shortcut record before the renderer reads
//! it. Registration order in `BrokerStore::register_subscription` is
//! the firing order for a single write (see `ox_broker` spec §3.3).

use std::collections::HashMap;
use std::sync::Arc;

use horns_core::subscription::{PathChange, PathPattern, SubCtx, Subscription, SubscriptionId};
use horns_core::{
    BindingEntry, BindingRegistry, CommandMetadata, CommandRegistry, Write as HornsWrite,
};
use ox_path::oxpath;
use ox_types::KeyHint;
use structfs_core_store::{Path, Reader, Record};

use crate::key_chord_canonical::encode_keychord_to_str;
use crate::settings::commands::account_model::path_ancestors;

/// Path at which the active shortcut set lives. Settings-scoped today;
/// when other horns-owned screens land their own resolvers, each gets
/// its own path under `ui/<screen>/shortcuts`.
pub fn shortcuts_path() -> Path {
    oxpath!("ui", "settings", "shortcuts")
}

/// Subscription that watches the focus cursor and re-projects the
/// active shortcut record on every move. Holds in-memory snapshots of
/// the binding + command registries captured at install time — those
/// never change at runtime, and re-reading them through the broker is
/// what made the original feature pathological.
pub struct ShortcutResolver {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    cursor_path: Path,
    output_path: Path,
    /// In-memory copy of every registered binding. Cloned out of the
    /// `BindingRegistry` at install time before it's moved into horns'
    /// side-tables. Stable for the lifetime of the subscription.
    bindings: Vec<BindingEntry>,
    /// `command_id → metadata` lookup used to attach display names to
    /// the projected `KeyHint`s. Built once from the `CommandRegistry`
    /// at install time.
    commands: HashMap<String, CommandMetadata>,
}

impl ShortcutResolver {
    /// Construct from the live registries — called at install time
    /// when both registries are still owned locally, before they're
    /// moved into the horns install bundle.
    pub fn from_registries(
        cursor_path: Path,
        output_path: Path,
        bindings: &BindingRegistry,
        commands: &CommandRegistry,
    ) -> Self {
        let binding_snapshot: Vec<BindingEntry> = bindings.entries().to_vec();
        let mut command_snapshot: HashMap<String, CommandMetadata> =
            HashMap::with_capacity(commands.iter().count());
        for cmd in commands.iter() {
            command_snapshot.insert(
                cmd.id().0.clone(),
                CommandMetadata {
                    display: cmd.display().clone(),
                    scope: cmd.scope().clone(),
                },
            );
        }
        Self {
            id: SubscriptionId("ox_cli.settings.shortcut_resolver".to_string()),
            watches: vec![PathPattern::Exact(cursor_path.clone())],
            cursor_path,
            output_path,
            bindings: binding_snapshot,
            commands: command_snapshot,
        }
    }

    pub fn boxed(self) -> Arc<dyn Subscription> {
        Arc::new(self)
    }
}

impl Subscription for ShortcutResolver {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<HornsWrite> {
        // Decode the cursor from the change. Prefer `after` (the new
        // value); fall back to a snapshot read of the cursor path only
        // when the change carries nothing — defensive against weird
        // unset transitions. Both are cheap leaf reads.
        let cursor = match cursor_from_change(ctx.change)
            .or_else(|| read_cursor(ctx.snapshot, &self.cursor_path))
        {
            Some(p) => p,
            None => return Vec::new(),
        };

        // Pure in-memory projection: no broker subtree reads. The
        // bindings + commands snapshots were captured at install time.
        let hints = project_for_cursor(&self.bindings, &self.commands, &cursor);

        let value = match structfs_serde_store::to_value(&hints) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        vec![HornsWrite {
            path: self.output_path.clone(),
            record: Record::parsed(value),
        }]
    }
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// Build the shortcut record for `cursor`: walk the cursor's ancestor
/// chain (innermost → outermost) once, emit one `KeyHint` per (key,
/// command) pair the dispatcher could reach from that scope, dedupe by
/// key (first-seen wins, matching the dispatcher's resolution order),
/// then sort by curated priority ascending so consumers can take the
/// top-N for compact displays.
fn project_for_cursor(
    bindings: &[BindingEntry],
    commands: &HashMap<String, CommandMetadata>,
    cursor: &Path,
) -> Vec<KeyHint> {
    let mut out: Vec<KeyHint> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let ancestors = path_ancestors(cursor);
    for scope_path in ancestors.iter().rev() {
        for entry in bindings {
            if !entry.scope.matches(scope_path) {
                continue;
            }
            let Some(wire) = encode_keychord_to_str(&entry.key) else {
                continue;
            };
            if !seen.insert(wire.clone()) {
                continue;
            }
            let Some(meta) = commands.get(&entry.command_id.0) else {
                continue;
            };
            out.push(KeyHint {
                key: wire,
                description: meta.display.name.clone(),
                command: entry.command_id.0.clone(),
                status_hint: false,
                priority: entry.priority,
            });
        }
    }
    out.sort_by_key(|h| h.priority);
    out
}

// ---------------------------------------------------------------------------
// Cursor decode helpers
// ---------------------------------------------------------------------------

fn cursor_from_change(change: &PathChange) -> Option<Path> {
    let after = change.after.as_ref()?;
    let value = after.as_value()?;
    crate::settings::commands::navigation::path_from_value(value)
}

fn read_cursor(data: &mut dyn Reader, cursor_path: &Path) -> Option<Path> {
    let rec = data.read(cursor_path).ok().flatten()?;
    let value = rec.as_value()?;
    crate::settings::commands::navigation::path_from_value(value)
}
