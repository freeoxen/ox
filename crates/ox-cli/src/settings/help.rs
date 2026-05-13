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
use crate::settings::binding_registry::BindingRegistry;
use crate::settings::command_registry::CommandRegistry;

/// Build the hint list for the given settings dispatch context.
///
/// Mirrors the dispatcher's three-pass lookup: edit-mode synthetic
/// cursor (when active) → focused row → page cursor. A key seen at
/// a higher-priority scope shadows the same key at a lower-priority
/// one, so the modal shows the binding that would actually fire.
///
/// The legacy single-cursor entrypoint was missing per-row Prefix
/// bindings (t / r / h / l / a / d / P) and edit-mode bindings — they
/// were never visible in the shortcuts modal because the page cursor
/// (`settings/index`) doesn't match `Prefix(settings/accounts)` or
/// `Exact(settings/_edit_mode)`. Threading the focused row + edit
/// flag fixes that.
pub fn key_hints_for_context(
    bindings: &BindingRegistry,
    commands: &CommandRegistry,
    page_cursor: &Path,
    focused: Option<&Path>,
    edit_mode: bool,
) -> Vec<KeyHint> {
    let mut out: Vec<KeyHint> = Vec::new();
    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let edit_scope;
    if edit_mode {
        edit_scope = ox_path::oxpath!("settings", "_edit_mode");
        emit_for_cursor(bindings, commands, &edit_scope, &mut seen_keys, &mut out);
    }
    if let Some(focus) = focused {
        emit_for_cursor(bindings, commands, focus, &mut seen_keys, &mut out);
    }
    emit_for_cursor(bindings, commands, page_cursor, &mut seen_keys, &mut out);
    out
}

/// Compatibility shim: hints for a page cursor only, no focused-row
/// or edit-mode context. Kept for tests; production callers now use
/// `key_hints_for_context`.
pub fn key_hints_for_cursor(
    bindings: &BindingRegistry,
    commands: &CommandRegistry,
    cursor: &Path,
) -> Vec<KeyHint> {
    key_hints_for_context(bindings, commands, cursor, None, false)
}

fn emit_for_cursor(
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

#[cfg(test)]
mod tests {
    use super::*;

    use ox_path::oxpath;
    use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};
    use ox_types::{BindingEntry, BindingScope, CommandId, KeyChord, Phase, Screen};

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
            screen: Screen::Settings,
            scope: BindingScope::Exact(oxpath!("settings", "index")),
            mode: None,
            key: key('?'),
            command_id: CommandId(String::from("highlight.index.next")),
            phase: Phase::Target,
        });
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
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
