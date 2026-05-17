//! Project the settings binding + command registries into the
//! `KeyHint` stream the shortcuts modal renders.
//!
//! The modal reads `vs.key_hints` and shows one row per hint. For
//! legacy screens those hints come from `InputStore` at
//! `input/bindings/{mode}/{screen}`. The new settings pipeline keeps
//! its bindings + command descriptions in a separate pair of
//! registries owned by the event loop, so the projection has to
//! happen client-side. This module is that projection.
//!
//! Scoping: at a given cursor we want every binding whose
//! `BindingScope` admits that cursor — `Anywhere` always, `Exact(p)`
//! when `p == cursor`, `Prefix(p)` when `cursor` starts with `p`'s
//! components. When several bindings share a key the most-specific
//! one wins (matching the dispatch lookup's resolution order); the
//! registry sorts entries that way already, so a single pass plus a
//! key-dedupe is enough.

use ox_types::KeyHint;
use structfs_core_store::Path;

use crate::key_chord_canonical::encode_keychord_to_str;
use crate::settings::BindingRegistry;
use crate::settings::CommandRegistry;
use horns_core::{BindingEntry, CommandMetadata};
use ox_broker::ClientHandle;

/// Build the hint list for the given settings dispatch context.
///
/// Mirrors the dispatcher's three-pass lookup: focused row (when set,
/// possibly itself `settings/_edit` for inline edit mode) → page
/// cursor. A key seen at a higher-priority scope shadows the same key
/// at a lower-priority one, so the modal shows the binding that would
/// actually fire.
///
/// The legacy single-cursor entrypoint was missing per-row Prefix
/// bindings (t / r / h / l / a / d / P) and edit-mode bindings — they
/// were never visible in the shortcuts modal because the page cursor
/// (`settings/index`) doesn't match `Prefix(settings/accounts)` or
/// `Exact(settings/_edit)`. Threading the focused row fixes both:
/// under cursor-as-focus the focused cursor moves to `settings/_edit`
/// when edit mode is active, so a single `focused`-handed parameter
/// covers both row-Prefix bindings and edit-mode bindings.
pub fn key_hints_for_context(
    bindings: &BindingRegistry,
    commands: &CommandRegistry,
    page_cursor: &Path,
    focused: Option<&Path>,
) -> Vec<KeyHint> {
    let mut out: Vec<KeyHint> = Vec::new();
    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(focus) = focused {
        emit_for_cursor(bindings, commands, focus, &mut seen_keys, &mut out);
    }
    emit_for_cursor(bindings, commands, page_cursor, &mut seen_keys, &mut out);
    out
}

/// Compatibility shim: hints for a page cursor only, no focused-row
/// context. Kept for tests; production callers now use
/// `key_hints_for_context`.
pub fn key_hints_for_cursor(
    bindings: &BindingRegistry,
    commands: &CommandRegistry,
    cursor: &Path,
) -> Vec<KeyHint> {
    key_hints_for_context(bindings, commands, cursor, None)
}

fn emit_for_cursor(
    bindings: &BindingRegistry,
    commands: &CommandRegistry,
    cursor: &Path,
    seen_keys: &mut std::collections::HashSet<String>,
    out: &mut Vec<KeyHint>,
) {
    // Walk the cursor's ancestor chain so a binding registered at an
    // outer scope (e.g. `Exact(settings)` j/k row-nav) shows up in
    // hints even when the focused cursor sits at a deeper row path
    // (e.g. `settings/accounts`). Mirrors `compute_scope_path` in
    // the dispatcher — what dispatch can reach, hints should expose.
    //
    // Walk inner → outer so a more-specific binding (deeper scope)
    // claims the key's hint row before an outer-scope binding for the
    // same key gets a chance. Mirrors the dispatcher's resolution
    // order for the Target / Capture phases.
    let ancestors = crate::settings::commands::account_model::path_ancestors(cursor);
    for scope_path_entry in ancestors.iter().rev() {
        emit_for_scope_path_entry(bindings, commands, scope_path_entry, seen_keys, out);
    }
}

fn emit_for_scope_path_entry(
    bindings: &BindingRegistry,
    commands: &CommandRegistry,
    cursor: &Path,
    seen_keys: &mut std::collections::HashSet<String>,
    out: &mut Vec<KeyHint>,
) {
    for entry in bindings.entries() {
        if !entry.scope.matches(cursor) {
            continue;
        }
        let Some(wire) = encode_keychord_to_str(&entry.key) else {
            continue;
        };
        if !seen_keys.insert(wire.clone()) {
            continue;
        }
        let Some(command) = commands.lookup(&entry.command_id) else {
            continue;
        };
        let display = command.display();
        out.push(KeyHint {
            key: wire,
            description: display.name.clone(),
            command: entry.command_id.0.clone(),
            status_hint: false,
        });
    }
}

// ---------------------------------------------------------------------------
// Broker-driven hints — reads BindingEntry + CommandMetadata from the
// broker subtrees populated at install time. Eliminates the need for the
// event loop to construct duplicate registries every frame.
// ---------------------------------------------------------------------------

/// Build the hint list by reading `<bindings_prefix>` + `<commands_prefix>`
/// from the broker, matching the in-process projection above. Used by the
/// event loop after the registries moved entirely into horns' side-tables.
pub async fn key_hints_for_context_from_broker(
    client: &ClientHandle,
    bindings_prefix: &Path,
    commands_prefix: &Path,
    page_cursor: &Path,
    focused: Option<&Path>,
) -> Vec<KeyHint> {
    // Read both subtrees up-front. Bindings + commands are small (tens
    // of entries) so the cost is negligible per frame.
    let binding_rows: Vec<BindingEntry> = match client.read_subtree(bindings_prefix).await {
        Ok(map) => map
            .into_values()
            .filter_map(|rec| {
                rec.as_value()
                    .cloned()
                    .and_then(|v| structfs_serde_store::from_value::<BindingEntry>(v).ok())
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    let mut commands_by_id: std::collections::HashMap<String, CommandMetadata> =
        std::collections::HashMap::new();
    if let Ok(map) = client.read_subtree(commands_prefix).await {
        for (path, rec) in map.into_iter() {
            let Some(id_component) = path.components.last() else {
                continue;
            };
            let id = id_component.as_str().to_string();
            let Some(value) = rec.as_value().cloned() else {
                continue;
            };
            if let Ok(meta) = structfs_serde_store::from_value::<CommandMetadata>(value) {
                commands_by_id.insert(id, meta);
            }
        }
    }

    let mut out: Vec<KeyHint> = Vec::new();
    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let scope_walk = |target: &Path,
                      seen_keys: &mut std::collections::HashSet<String>,
                      out: &mut Vec<KeyHint>| {
        let ancestors = crate::settings::commands::account_model::path_ancestors(target);
        for scope_path_entry in ancestors.iter().rev() {
            for entry in &binding_rows {
                if !entry.scope.matches(scope_path_entry) {
                    continue;
                }
                let Some(wire) = encode_keychord_to_str(&entry.key) else {
                    continue;
                };
                if !seen_keys.insert(wire.clone()) {
                    continue;
                }
                let Some(meta) = commands_by_id.get(&entry.command_id.0) else {
                    continue;
                };
                out.push(KeyHint {
                    key: wire,
                    description: meta.display.name.clone(),
                    command: entry.command_id.0.clone(),
                    status_hint: false,
                });
            }
        }
    };
    if let Some(focus) = focused {
        scope_walk(focus, &mut seen_keys, &mut out);
    }
    scope_walk(page_cursor, &mut seen_keys, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use ox_path::oxpath;
    use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};
    use ox_types::{BindingEntry, BindingScope, CommandId, KeyChord, Phase};

    use crate::settings::commands::register_all as register_all_commands;

    fn key(c: char) -> KeyChord {
        KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char(c),
        }
    }

    fn populated_registries() -> (BindingRegistry, CommandRegistry) {
        let mut bindings = BindingRegistry::new();
        crate::settings::bindings::register(&mut bindings);
        let mut commands = CommandRegistry::new();
        register_all_commands(&mut commands);
        (bindings, commands)
    }

    #[test]
    fn cursor_specific_and_whole_screen_both_appear_for_index() {
        let (bindings, commands) = populated_registries();
        let hints = key_hints_for_cursor(&bindings, &commands, &oxpath!("settings", "index"));

        // The index page binds j/k/Enter/Esc at cursor scope.
        assert!(
            hints
                .iter()
                .any(|h| h.key == "j" && h.command == "tree.next")
        );
        assert!(
            hints
                .iter()
                .any(|h| h.key == "Enter" && h.command == "tree.activate")
        );
        assert!(
            hints
                .iter()
                .any(|h| h.key == "Esc" && h.command == "tree.collapse_or_ascend")
        );
        // The whole-screen `?` is visible at every cursor.
        assert!(
            hints
                .iter()
                .any(|h| h.key == "?" && h.command == "modal.toggle_shortcuts")
        );
    }

    #[test]
    fn cursor_specific_binding_shadows_whole_screen_for_same_key() {
        // Construct a registry where `?` is whole-screen AND cursor-bound;
        // the cursor-bound entry registers first so it wins both by
        // specificity and registration order.
        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "index")),
            key: key('?'),
            command_id: CommandId(String::from("highlight.index.next")),
            phase: Phase::Target,
        });
        bindings.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key('?'),
            command_id: CommandId(String::from("modal.toggle_shortcuts")),
            phase: Phase::Target,
        });
        let mut commands = CommandRegistry::new();
        register_all_commands(&mut commands);

        let hints = key_hints_for_cursor(&bindings, &commands, &oxpath!("settings", "index"));
        let q = hints.iter().filter(|h| h.key == "?").count();
        assert_eq!(q, 1, "duplicate keys must dedupe to one row");
        let qhint = hints.iter().find(|h| h.key == "?").unwrap();
        assert_eq!(qhint.command, "highlight.index.next");
    }

    #[test]
    fn keys_from_other_cursor_pages_do_not_leak() {
        let (bindings, commands) = populated_registries();
        // Models page binds `r` (account.refresh); it must NOT appear
        // when the cursor is on the index.
        let hints = key_hints_for_cursor(&bindings, &commands, &oxpath!("settings", "index"));
        assert!(
            hints.iter().all(|h| h.key != "r"),
            "models-only `r` should not show on the index hint list"
        );
    }
}
