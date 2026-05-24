//! Active-shortcut registry for the settings screen.
//!
//! For the current focus cursor, this module maintains a single broker
//! record — a `Vec<KeyHint>` filtered to the bindings that are reachable
//! from the cursor's scope chain, deduped per key, and sorted by curated
//! priority. Consumers (the footer bar, the shortcuts modal, anything
//! later) read that one record; nobody re-projects per frame.
//!
//! The registry updates when the cursor moves *or* when the binding /
//! command registries change. Three watches:
//!
//! - `PathPattern::Exact(cursor_path)` — re-project against the current
//!   cached snapshot. The hot path: ~µs per cursor move.
//! - `PathPattern::Prefix(bindings_prefix)` — invalidate snapshot,
//!   rebuild from the broker on the next project. Fires only when a
//!   binding is added / changed / removed (rare; user-defined dynamic
//!   shortcuts will land here).
//! - `PathPattern::Prefix(commands_prefix)` — same shape for command
//!   metadata.
//!
//! The cache is what makes this affordable. `LocalConfig::read` on a
//! non-leaf is O(total store keys) — reading the bindings subtree from
//! scratch on every cursor move pegged CPU. Caching the projection
//! inputs and rebuilding only on actual binding writes keeps the hot
//! path purely in-memory.
//!
//! Initial snapshot is taken at install time from the live
//! `BindingRegistry` / `CommandRegistry`, so the first cursor move
//! doesn't pay the broker round-trip just to learn what it already
//! had in hand.
//!
//! Ordering note: this subscription is registered *before* horns'
//! `RenderSubscription` so that when a cursor write fires both, the
//! resolver writes the fresh shortcut record before the renderer reads
//! it. Registration order in `BrokerStore::register_subscription` is
//! the firing order for a single write (see `ox_broker` spec §3.3).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use horns_core::subscription::{PathChange, PathPattern, SubCtx, Subscription, SubscriptionId};
use horns_core::{
    BindingEntry, BindingRegistry, CommandMetadata, CommandRegistry, Write as HornsWrite,
};
use ox_path::oxpath;
use ox_types::KeyHint;
use structfs_core_store::{Path, Record, Reader, Value};

use crate::key_chord_canonical::encode_keychord_to_str;
use crate::settings::commands::account_model::path_ancestors;

/// Path at which the active shortcut set lives. Settings-scoped today;
/// when other horns-owned screens land their own resolvers, each gets
/// its own path under `ui/<screen>/shortcuts`.
pub fn shortcuts_path() -> Path {
    oxpath!("ui", "settings", "shortcuts")
}

/// Cached projection inputs. `valid: false` means a binding/command
/// write fired since the last project; the next handle invocation
/// re-reads both subtrees from the broker and sets `valid: true`.
struct Snapshot {
    bindings: Vec<BindingEntry>,
    commands: HashMap<String, CommandMetadata>,
    valid: bool,
}

/// Subscription that maintains the active shortcut record for the
/// settings screen. See module docs for the watch/invalidation model.
pub struct ShortcutResolver {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    cursor_path: Path,
    bindings_prefix: Path,
    commands_prefix: Path,
    output_path: Path,
    snapshot: Arc<RwLock<Snapshot>>,
}

impl ShortcutResolver {
    /// Construct from the live registries — called at install time
    /// when both registries are still owned locally, before they're
    /// moved into the horns install bundle. Seeds the snapshot with
    /// what install has in hand so the first cursor move doesn't pay
    /// a broker round-trip.
    pub fn from_registries(
        cursor_path: Path,
        bindings_prefix: Path,
        commands_prefix: Path,
        output_path: Path,
        bindings: &BindingRegistry,
        commands: &CommandRegistry,
    ) -> Self {
        let mut command_snapshot: HashMap<String, CommandMetadata> = HashMap::new();
        for cmd in commands.iter() {
            command_snapshot.insert(
                cmd.id().0.clone(),
                CommandMetadata {
                    display: cmd.display().clone(),
                    scope: cmd.scope().clone(),
                },
            );
        }
        let snapshot = Snapshot {
            bindings: bindings.entries().to_vec(),
            commands: command_snapshot,
            valid: true,
        };
        Self {
            id: SubscriptionId("ox_cli.settings.shortcut_resolver".to_string()),
            watches: vec![
                PathPattern::Exact(cursor_path.clone()),
                PathPattern::Prefix(bindings_prefix.clone()),
                PathPattern::Prefix(commands_prefix.clone()),
            ],
            cursor_path,
            bindings_prefix,
            commands_prefix,
            output_path,
            snapshot: Arc::new(RwLock::new(snapshot)),
        }
    }

    pub fn boxed(self) -> Arc<dyn Subscription> {
        Arc::new(self)
    }

    /// True iff `change_path` sits under one of the tracked subtrees
    /// (bindings or commands). Used to decide whether the snapshot
    /// needs to be marked stale before we re-project.
    fn change_invalidates_snapshot(&self, change_path: &Path) -> bool {
        is_component_prefix(&self.bindings_prefix, change_path)
            || is_component_prefix(&self.commands_prefix, change_path)
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
        // Step 1: invalidate snapshot if a binding / command write
        // fired us. Cursor-driven fires leave `valid` alone.
        if self.change_invalidates_snapshot(&ctx.change.path) {
            self.snapshot
                .write()
                .expect("shortcut snapshot poisoned")
                .valid = false;
        }

        // Step 2: rebuild snapshot if needed. The recursive subtree
        // reads here only run after an invalidation — typically once
        // per binding edit, not per cursor move. Take the write lock
        // for the rebuild, then drop it before projection so a future
        // cache hit for an unrelated cursor move can take a read lock.
        {
            let needs_rebuild = !self
                .snapshot
                .read()
                .expect("shortcut snapshot poisoned")
                .valid;
            if needs_rebuild {
                let bindings = read_bindings(ctx.snapshot, &self.bindings_prefix);
                let commands = read_commands(ctx.snapshot, &self.commands_prefix);
                let mut guard = self
                    .snapshot
                    .write()
                    .expect("shortcut snapshot poisoned");
                guard.bindings = bindings;
                guard.commands = commands;
                guard.valid = true;
            }
        }

        // Step 3: decode current cursor. When the firing write IS the
        // cursor, prefer `change.after` (we have the new value in
        // hand). When the fire came from a binding/command change,
        // `change.after` holds the binding payload — fall back to a
        // leaf read of `cursor_path`.
        let cursor = match cursor_from_change(ctx.change)
            .or_else(|| read_cursor(ctx.snapshot, &self.cursor_path))
        {
            Some(p) => p,
            None => return Vec::new(),
        };

        // Step 4: project against the (now-valid) cached snapshot.
        let guard = self
            .snapshot
            .read()
            .expect("shortcut snapshot poisoned");
        let hints = project_for_cursor(&guard.bindings, &guard.commands, &cursor);
        drop(guard);

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
// Snapshot rebuild — runs only after an invalidation, never on the
// per-cursor hot path. Single Map-read per subtree, then deserialize
// the values in place (no N+1 child reads).
// ---------------------------------------------------------------------------

fn read_bindings(data: &mut dyn Reader, prefix: &Path) -> Vec<BindingEntry> {
    let Ok(Some(parent_rec)) = data.read(prefix) else {
        return Vec::new();
    };
    let Some(Value::Map(map)) = parent_rec.as_value() else {
        return Vec::new();
    };
    map.values()
        .filter_map(|v| structfs_serde_store::from_value::<BindingEntry>(v.clone()).ok())
        .collect()
}

fn read_commands(
    data: &mut dyn Reader,
    prefix: &Path,
) -> HashMap<String, CommandMetadata> {
    let Ok(Some(parent_rec)) = data.read(prefix) else {
        return HashMap::new();
    };
    let Some(Value::Map(map)) = parent_rec.as_value() else {
        return HashMap::new();
    };
    map.iter()
        .filter_map(|(name, v)| {
            structfs_serde_store::from_value::<CommandMetadata>(v.clone())
                .ok()
                .map(|meta| (name.clone(), meta))
        })
        .collect()
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

/// Component-wise prefix check: true iff `prefix.components` is a
/// prefix of `whole.components` (a path is a prefix of itself).
/// Mirrors horns-core's internal helper without taking a dep on a
/// private item.
fn is_component_prefix(prefix: &Path, whole: &Path) -> bool {
    if prefix.components.len() > whole.components.len() {
        return false;
    }
    whole.components[..prefix.components.len()] == prefix.components[..]
}
