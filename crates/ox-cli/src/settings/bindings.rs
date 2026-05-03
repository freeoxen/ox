//! Day-one binding registrations for the settings screen.
//!
//! Per spec §6 the settings screen exposes a small fixed binding table per
//! cursor scope. Each scope registers its own keys via plain
//! `BindingEntry { ... }` literals — flat clarity over indirection.
//!
//! Text-editing scopes (currently only `settings/accounts/_detail`) get a
//! single helper that registers ~95 entries: one per printable ASCII
//! character mapped to `field.insert`, plus a Backspace mapped to
//! `field.delete_back`. The character payload is consumed by `field.insert`
//! through `ctx.last_keystroke`.

use ox_path::oxpath;
use structfs_core_store::Path;

use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};
use ox_types::{BindingEntry, BindingScope, CommandId, KeyChord, Screen};

use crate::settings::binding_registry::BindingRegistry;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn no_mods() -> KeyModifierSet {
    KeyModifierSet::default()
}

fn shift_only() -> KeyModifierSet {
    KeyModifierSet {
        shift: true,
        ..KeyModifierSet::default()
    }
}

fn ctrl_only() -> KeyModifierSet {
    KeyModifierSet {
        ctrl: true,
        ..KeyModifierSet::default()
    }
}

fn cmd(id: &str) -> CommandId {
    CommandId(String::from(id))
}

fn bind(
    reg: &mut BindingRegistry,
    cursor: Option<Path>,
    modifiers: KeyModifierSet,
    code: KeyCodeRepr,
    command_id: &str,
) {
    let scope = match cursor {
        Some(p) => BindingScope::Exact(p),
        None => BindingScope::Anywhere,
    };
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope,
        mode: None,
        key: KeyChord { modifiers, code },
        command_id: cmd(command_id),
    });
}

/// Bind a key under a `Prefix` scope — fires when the cursor sits at
/// `prefix` itself or any deeper component path. Used by per-row
/// commands that act on a focused subtree (e.g. `t` testing whichever
/// account is currently focused at `settings/accounts/{any}`).
fn bind_prefix(
    reg: &mut BindingRegistry,
    prefix: Path,
    modifiers: KeyModifierSet,
    code: KeyCodeRepr,
    command_id: &str,
) {
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Prefix(prefix),
        mode: None,
        key: KeyChord { modifiers, code },
        command_id: cmd(command_id),
    });
}

/// Register every printable-ASCII-char insert + Backspace delete-back
/// binding for a text-editing cursor scope.
///
/// `field.insert` reads the actual character from `ctx.last_keystroke`, so
/// a single command id services every printable key. Day-one this covers
/// the account-detail page; future text-editing scopes (model id editor,
/// new-account name input) call this with their own cursor path.
fn register_text_editing(reg: &mut BindingRegistry, cursor: Path) {
    // Printable ASCII (0x20..=0x7E inclusive — 95 chars).
    for byte in 0x20u8..=0x7E {
        let ch = byte as char;
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(cursor.clone()),
            mode: None,
            key: KeyChord {
                modifiers: no_mods(),
                code: KeyCodeRepr::Char(ch),
            },
            command_id: cmd("field.insert"),
        });
    }
    // Backspace.
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(cursor),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Backspace,
        },
        command_id: cmd("field.delete_back"),
    });
}

// ---------------------------------------------------------------------------
// Per-scope registration
// ---------------------------------------------------------------------------

fn register_index(reg: &mut BindingRegistry) {
    let cursor = oxpath!("settings", "index");
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Char('j'),
        "tree.next",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Down,
        "tree.next",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Char('k'),
        "tree.prev",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Up,
        "tree.prev",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Enter,
        "tree.activate",
    );
    bind(
        reg,
        Some(cursor),
        no_mods(),
        KeyCodeRepr::Esc,
        "tree.collapse_or_ascend",
    );
}

fn register_account_detail(reg: &mut BindingRegistry) {
    let cursor = oxpath!("settings", "accounts", "_detail");
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Tab,
        "field.account.next",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Down,
        "field.account.next",
    );
    bind(
        reg,
        Some(cursor.clone()),
        shift_only(),
        KeyCodeRepr::BackTab,
        "field.account.prev",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Up,
        "field.account.prev",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Char('t'),
        "account.test",
    );
    bind(
        reg,
        Some(cursor.clone()),
        ctrl_only(),
        KeyCodeRepr::Char('s'),
        "app.save",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Esc,
        "nav.ascend",
    );
    // Printable chars + Backspace via the helper — these come last so the
    // scope's specific bindings (Tab, t, Ctrl+s, Esc) win over a literal
    // 't' or ' ' insert when the BindingEntry is identical aside from
    // command. Lookup uses the *first* registered match within a
    // specificity class, so registration order matters here.
    register_text_editing(reg, cursor);
}

fn register_account_new(reg: &mut BindingRegistry) {
    let cursor = oxpath!("settings", "accounts", "_new");
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Enter,
        "accounts.create",
    );
    bind(
        reg,
        Some(cursor),
        no_mods(),
        KeyCodeRepr::Esc,
        "accounts.cancel",
    );
}

fn register_account_delete(reg: &mut BindingRegistry) {
    let cursor = oxpath!("settings", "accounts", "_delete");
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Char('y'),
        "accounts.delete",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Char('n'),
        "accounts.cancel",
    );
    bind(
        reg,
        Some(cursor),
        no_mods(),
        KeyCodeRepr::Esc,
        "accounts.cancel",
    );
}

fn register_model_detail(reg: &mut BindingRegistry) {
    let cursor = oxpath!("settings", "models", "_detail");
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Tab,
        "field.model.next",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Down,
        "field.model.next",
    );
    bind(
        reg,
        Some(cursor.clone()),
        shift_only(),
        KeyCodeRepr::BackTab,
        "field.model.prev",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Up,
        "field.model.prev",
    );
    bind(
        reg,
        Some(cursor.clone()),
        shift_only(),
        KeyCodeRepr::Char('P'),
        "models.set_primary",
    );
    bind(reg, Some(cursor), no_mods(), KeyCodeRepr::Esc, "nav.ascend");
}

/// Per-row commands for accordion-focused leaf rows. Bound under
/// `Prefix(settings/accounts)` and `Prefix(settings/models)` so they
/// fire whenever the focused row sits anywhere inside that subtree —
/// `settings/accounts` (the parent), `settings/accounts/{name}`
/// (the leaf), or `settings/accounts/{name}/{field}` (the inline
/// field rows). The commands themselves read the focused row to
/// figure out *which* account/model to act on.
fn register_row_prefixes(reg: &mut BindingRegistry) {
    let accounts_subtree = oxpath!("settings", "accounts");
    bind_prefix(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('a'),
        "accounts.add",
    );
    bind_prefix(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('t'),
        "account.test",
    );
    bind_prefix(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('r'),
        "account.refresh",
    );
    bind_prefix(
        reg,
        accounts_subtree,
        no_mods(),
        KeyCodeRepr::Char('d'),
        "accounts.delete_confirm",
    );
    let models_subtree = oxpath!("settings", "models");
    bind_prefix(
        reg,
        models_subtree.clone(),
        shift_only(),
        KeyCodeRepr::Char('P'),
        "models.set_primary",
    );
    // `r` refreshes the focused model's owning account catalog. Useful
    // both when focused on an account row and when focused on a model
    // row (the latter is what the accordion makes natural).
    bind_prefix(
        reg,
        models_subtree,
        no_mods(),
        KeyCodeRepr::Char('r'),
        "account.refresh",
    );
}

/// Whole-screen `?` toggles the shortcuts modal regardless of cursor
/// depth. `BindingScope::Anywhere` means specific scopes can still
/// shadow it by registering a same-key binding (none do today).
fn register_global(reg: &mut BindingRegistry) {
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Anywhere,
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Char('?'),
        },
        command_id: cmd("modal.toggle_shortcuts"),
    });
}

/// Register every day-one settings binding into `reg`.
pub fn register(reg: &mut BindingRegistry) {
    register_global(reg);
    register_index(reg);
    register_account_detail(reg);
    register_account_new(reg);
    register_account_delete(reg);
    register_model_detail(reg);
    register_row_prefixes(reg);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn populated() -> BindingRegistry {
        let mut reg = BindingRegistry::new();
        register(&mut reg);
        reg
    }

    fn key(modifiers: KeyModifierSet, code: KeyCodeRepr) -> KeyChord {
        KeyChord { modifiers, code }
    }

    #[test]
    fn index_j_resolves_to_tree_next() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "index"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('j')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("tree.next"));
    }

    #[test]
    fn index_enter_resolves_to_tree_activate() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "index"),
                None,
                &key(no_mods(), KeyCodeRepr::Enter),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("tree.activate"));
    }

    #[test]
    fn index_esc_resolves_to_tree_collapse_or_ascend() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "index"),
                None,
                &key(no_mods(), KeyCodeRepr::Esc),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("tree.collapse_or_ascend"));
    }

    #[test]
    fn accounts_a_resolves_to_accounts_add() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('a')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("accounts.add"));
    }

    #[test]
    fn focused_account_row_t_resolves_to_account_test() {
        // Per-row prefix binding: `t` fires whenever the cursor sits
        // under `settings/accounts`, including the account leaf row
        // — no page-flip required.
        let reg = populated();
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts", comp),
                None,
                &key(no_mods(), KeyCodeRepr::Char('t')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("account.test"));
    }

    #[test]
    fn focused_account_row_r_resolves_to_account_refresh() {
        let reg = populated();
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts", comp),
                None,
                &key(no_mods(), KeyCodeRepr::Char('r')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("account.refresh"));
    }

    #[test]
    fn focused_model_row_p_resolves_to_set_primary() {
        let reg = populated();
        let acct = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        let model = ox_kernel::PathComponent::try_new("claude_haiku").unwrap();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "models", acct, model),
                None,
                &key(shift_only(), KeyCodeRepr::Char('P')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("models.set_primary"));
    }

    #[test]
    fn detail_t_resolves_to_account_test() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts", "_detail"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('t')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("account.test"));
    }

    #[test]
    fn detail_printable_char_resolves_to_field_insert() {
        let reg = populated();
        // 'x' is not bound to anything else on _detail, so it must hit
        // the text-editing helper.
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts", "_detail"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('x')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("field.insert"));
    }

    #[test]
    fn detail_backspace_resolves_to_field_delete_back() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts", "_detail"),
                None,
                &key(no_mods(), KeyCodeRepr::Backspace),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("field.delete_back"));
    }

    #[test]
    fn detail_unbound_chord_returns_none() {
        let reg = populated();
        let hit = reg.lookup(
            Screen::Settings,
            &oxpath!("settings", "accounts", "_detail"),
            None,
            &key(ctrl_only(), KeyCodeRepr::Char('x')),
        );
        assert!(hit.is_none());
    }

    #[test]
    fn models_capital_p_resolves_to_set_primary() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "models"),
                None,
                &key(shift_only(), KeyCodeRepr::Char('P')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("models.set_primary"));
    }

    #[test]
    fn detail_t_beats_text_editing_t() {
        // The scope-specific 't' binding (account.test) must win over the
        // generic text-editing 't' (field.insert) on _detail. They have
        // identical specificity (both cursor-Some / mode-None), so this
        // depends on registration order. account.test is registered
        // before register_text_editing, so the lookup picks it.
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts", "_detail"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('t')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("account.test"));
    }

    #[test]
    fn account_new_enter_resolves_to_create() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts", "_new"),
                None,
                &key(no_mods(), KeyCodeRepr::Enter),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("accounts.create"));
    }

    #[test]
    fn account_delete_y_resolves_to_delete() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts", "_delete"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('y')),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("accounts.delete"));
    }

    #[test]
    fn model_detail_tab_resolves_to_field_model_next() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "models", "_detail"),
                None,
                &key(no_mods(), KeyCodeRepr::Tab),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("field.model.next"));
    }

    #[test]
    fn every_registered_binding_round_trips_through_lookup() {
        // Every registered `BindingEntry` must resolve to *some* command via
        // `lookup`, exercised under its own `(screen, cursor, mode, key)`.
        // For the dominant case (most entries) lookup returns the entry's
        // own `command_id` — a strict round-trip. A small number of entries
        // are intentionally shadowed by an earlier-registered binding with
        // the same scope+key (e.g. `account.test` shadows the text-editing
        // `field.insert` for `t` on `_detail`); those lookup to the
        // shadowing command instead. Both outcomes are acceptable; a `None`
        // is not — that would mean the entry is structurally orphaned.
        let reg = populated();
        let entries = reg.entries();
        let empty_path = oxpath!();

        let mut directly_reachable = 0usize;
        let mut shadowed: Vec<(BindingEntry, CommandId)> = Vec::new();
        for entry in entries {
            let cursor = entry.scope.keyed_path().unwrap_or(&empty_path);
            let resolved = reg
                .lookup(entry.screen, cursor, entry.mode, &entry.key)
                .unwrap_or_else(|| {
                    panic!("binding {entry:?} resolved to None — structurally unreachable")
                });
            if resolved == &entry.command_id {
                directly_reachable += 1;
            } else {
                shadowed.push((entry.clone(), resolved.clone()));
            }
        }
        // Shadowing should be rare and the cause obvious: an earlier-
        // registered binding with the same scope+key wins. Anything
        // unexpected here means a registration ordering bug.
        for (entry, winner) in &shadowed {
            // The text-editing helper binds every printable ASCII char to
            // `field.insert`; a same-scope earlier binding (e.g. `t` →
            // `account.test`) shadows that entry. Anything *not* of that
            // shape is a real surprise.
            assert_eq!(
                entry.command_id.0, "field.insert",
                "unexpected shadowing: {entry:?} shadowed by {winner:?}"
            );
        }
        // Sanity: at least the bulk of day-one bindings round-trip directly.
        assert!(
            directly_reachable >= entries.len() - shadowed.len(),
            "internal counting mismatch"
        );
        assert!(
            directly_reachable > 100,
            "expected most entries to be directly reachable; got {directly_reachable} of {}",
            entries.len()
        );
    }
}
