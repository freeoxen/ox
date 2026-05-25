//! Active-shortcut registry for the settings screen.
//!
//! Two store-resident records:
//!
//! - **Joined registry** at `joined_registry_path()` — the full set of
//!   bindings with their commands' display info, in one flat `Vec`.
//!   Rebuilt only when bindings or commands actually change; seeded at
//!   install time from the live registries. Reading this is one cheap
//!   leaf read.
//! - **Active shortcut record** at `shortcuts_path()` — the joined
//!   registry filtered to the cursor's scope chain, deduped per key,
//!   sorted by curated priority. This is what consumers (footer bar,
//!   shortcuts modal) read.
//!
//! `ShortcutResolver` is the stateless subscription that maintains
//! both. It watches three patterns:
//!
//! - `Exact(cursor_path)` — read joined registry, project, write the
//!   shortcut record. Hot path; the only thing the cursor autorepeat
//!   ever triggers.
//! - `Prefix(bindings_prefix)` — rebuild joined registry from the
//!   bindings + commands subtrees, write both joined registry and a
//!   freshly-projected shortcut record.
//! - `Prefix(commands_prefix)` — same.
//!
//! Why a store-resident joined registry instead of a field on the
//! resolver: state belongs in stores, handlers are pure functions of
//! their inputs. No interior mutability, no `Mutex<Snapshot>`, no
//! "what happens if two cursor moves race the cache" question.
//!
//! Ordering note: this subscription is registered *before* horns'
//! `RenderSubscription` so that when a cursor write fires both, the
//! resolver writes the fresh shortcut record before the renderer reads
//! it. Registration order in `BrokerStore::register_subscription` is
//! the firing order for a single write (see `ox_broker` spec §3.3).

use std::sync::Arc;

use horns_core::subscription::{PathChange, PathPattern, SubCtx, Subscription, SubscriptionId};
use horns_core::{
    BindingEntry, BindingRegistry, BindingScope, CommandId, CommandRegistry, KeyChord,
    Write as HornsWrite,
};
use ox_path::oxpath;
use ox_types::KeyHint;
use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Reader, Record, Value};

use crate::key_chord_canonical::encode_keychord_to_str;
use crate::settings::commands::account_model::path_ancestors;

/// Path the resolver writes the cursor-filtered shortcut record to.
/// Consumers (footer bar, shortcuts modal) read this and nothing else.
pub fn shortcuts_path() -> Path {
    oxpath!("ui", "settings", "shortcuts")
}

/// Path holding the full binding × command join. Materialized once at
/// install time from the live registries, then refreshed by the
/// resolver whenever a write under `bindings_prefix` /
/// `commands_prefix` says it might have changed.
pub fn joined_registry_path() -> Path {
    oxpath!("horns", "settings", "shortcut_registry")
}

/// One entry in the joined registry. Carries everything projection
/// needs: dispatcher inputs (scope, key) and display inputs
/// (description, priority, command_id). Flat so a `Vec<JoinedBinding>`
/// reads in one leaf hit without any secondary lookups.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JoinedBinding {
    pub scope: BindingScope,
    pub key: KeyChord,
    pub priority: u8,
    pub description: String,
    pub command_id: CommandId,
}

/// Build the joined registry from the live in-memory registries. Used
/// at install time so the first write to `joined_registry_path()`
/// happens with the data install already has in hand — the resolver
/// doesn't need to do a recursive read of the bindings subtree just
/// to learn what was just written to it.
pub fn build_joined_from_registries(
    bindings: &BindingRegistry,
    commands: &CommandRegistry,
) -> Vec<JoinedBinding> {
    let mut name_by_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for cmd in commands.iter() {
        name_by_id.insert(cmd.id().0.clone(), cmd.display().name.clone());
    }
    bindings
        .entries()
        .iter()
        .filter_map(|entry| {
            let description = name_by_id.get(&entry.command_id.0)?.clone();
            Some(JoinedBinding {
                scope: entry.scope.clone(),
                key: entry.key.clone(),
                priority: entry.priority,
                description,
                command_id: entry.command_id.clone(),
            })
        })
        .collect()
}

/// Stateless subscription that maintains both the joined registry
/// (only on binding/command edits) and the cursor-filtered shortcut
/// record (on every cursor move). See module docs for the watch model.
pub struct ShortcutResolver {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    cursor_path: Path,
    bindings_prefix: Path,
    commands_prefix: Path,
    registry_path: Path,
    output_path: Path,
}

impl ShortcutResolver {
    pub fn new(
        cursor_path: Path,
        bindings_prefix: Path,
        commands_prefix: Path,
        registry_path: Path,
        output_path: Path,
    ) -> Self {
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
            registry_path,
            output_path,
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
        let mut writes: Vec<HornsWrite> = Vec::new();

        // A binding or command write means the joined registry might
        // be stale. Rebuild from the bindings/commands subtrees and
        // queue a write so the registry path holds the new join.
        let registry_changed = is_component_prefix(&self.bindings_prefix, &ctx.change.path)
            || is_component_prefix(&self.commands_prefix, &ctx.change.path);

        let joined: Vec<JoinedBinding> = if registry_changed {
            let fresh = build_joined_from_broker(
                ctx.snapshot,
                &self.bindings_prefix,
                &self.commands_prefix,
            );
            if let Ok(value) = structfs_serde_store::to_value(&fresh) {
                writes.push(HornsWrite {
                    path: self.registry_path.clone(),
                    record: Record::parsed(value),
                });
            }
            fresh
        } else {
            // Cursor change — the joined registry didn't change, so
            // we read it from the store rather than rebuilding.
            read_joined(ctx.snapshot, &self.registry_path)
        };

        // Decode the current cursor. When the firing write IS the
        // cursor, `change.after` is the new value — use it directly.
        // Otherwise read the cursor path; the binding/command write
        // didn't carry a cursor.
        let cursor = match cursor_from_change(ctx.change)
            .or_else(|| read_cursor(ctx.snapshot, &self.cursor_path))
        {
            Some(p) => p,
            None => return writes,
        };

        let hints = project_for_cursor(&joined, &cursor);
        if let Ok(value) = structfs_serde_store::to_value(&hints) {
            writes.push(HornsWrite {
                path: self.output_path.clone(),
                record: Record::parsed(value),
            });
        }
        writes
    }
}

// ---------------------------------------------------------------------------
// Projection (pure)
// ---------------------------------------------------------------------------

/// Build the shortcut record for `cursor`: walk the cursor's ancestor
/// chain (innermost → outermost) once, emit one `KeyHint` per (key,
/// command) pair the dispatcher could reach from that scope, dedupe by
/// key (first-seen wins, matching the dispatcher's resolution order),
/// then sort by curated priority ascending so consumers can take the
/// top-N for compact displays.
fn project_for_cursor(joined: &[JoinedBinding], cursor: &Path) -> Vec<KeyHint> {
    let mut out: Vec<KeyHint> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let ancestors = path_ancestors(cursor);
    for scope_path in ancestors.iter().rev() {
        for entry in joined {
            if !entry.scope.matches(scope_path) {
                continue;
            }
            let Some(wire) = encode_keychord_to_str(&entry.key) else {
                continue;
            };
            if !seen.insert(wire.clone()) {
                continue;
            }
            out.push(KeyHint {
                key: wire,
                description: entry.description.clone(),
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
// Snapshot rebuild — only on binding/command writes, never on the
// per-cursor hot path. One Map-read per subtree, then deserialize the
// values in place (no N+1 child reads).
// ---------------------------------------------------------------------------

fn build_joined_from_broker(
    data: &mut dyn Reader,
    bindings_prefix: &Path,
    commands_prefix: &Path,
) -> Vec<JoinedBinding> {
    let bindings = read_subtree_values::<BindingEntry>(data, bindings_prefix);
    let commands = read_commands_index(data, commands_prefix);
    bindings
        .into_iter()
        .filter_map(|entry| {
            let description = commands.get(&entry.command_id.0)?.clone();
            Some(JoinedBinding {
                scope: entry.scope,
                key: entry.key,
                priority: entry.priority,
                description,
                command_id: entry.command_id,
            })
        })
        .collect()
}

fn read_subtree_values<T: for<'de> Deserialize<'de>>(
    data: &mut dyn Reader,
    prefix: &Path,
) -> Vec<T> {
    let Ok(Some(parent_rec)) = data.read(prefix) else {
        return Vec::new();
    };
    let Some(Value::Map(map)) = parent_rec.as_value() else {
        return Vec::new();
    };
    map.values()
        .filter_map(|v| structfs_serde_store::from_value::<T>(v.clone()).ok())
        .collect()
}

/// Read the commands subtree as `command_id → display_name`. We only
/// keep the display name because that's all the join needs; the rest
/// of `CommandMetadata` (scope) is unused in projection.
fn read_commands_index(
    data: &mut dyn Reader,
    prefix: &Path,
) -> std::collections::HashMap<String, String> {
    let Ok(Some(parent_rec)) = data.read(prefix) else {
        return std::collections::HashMap::new();
    };
    let Some(Value::Map(map)) = parent_rec.as_value() else {
        return std::collections::HashMap::new();
    };
    map.iter()
        .filter_map(|(name, v)| {
            structfs_serde_store::from_value::<horns_core::CommandMetadata>(v.clone())
                .ok()
                .map(|meta| (name.clone(), meta.display.name))
        })
        .collect()
}

fn read_joined(data: &mut dyn Reader, path: &Path) -> Vec<JoinedBinding> {
    let Ok(Some(rec)) = data.read(path) else {
        return Vec::new();
    };
    let Some(value) = rec.as_value() else {
        return Vec::new();
    };
    structfs_serde_store::from_value::<Vec<JoinedBinding>>(value.clone()).unwrap_or_default()
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

/// Component-wise prefix check: true iff `prefix` is a
/// prefix of `whole` (a path is a prefix of itself).
fn is_component_prefix(prefix: &Path, whole: &Path) -> bool {
    whole.has_prefix(prefix)
}
