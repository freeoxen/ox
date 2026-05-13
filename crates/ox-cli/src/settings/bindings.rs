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
use ox_types::{BindingEntry, BindingScope, CommandId, KeyChord, Phase, Screen};

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
        phase: Phase::Target,
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
        phase: Phase::Target,
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
            phase: Phase::Target,
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
        phase: Phase::Target,
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
    // Vim aliases: `e` (edit), `o` (open), `i` (insert) all route
    // to `tree.activate`. `tree.activate` already dispatches by
    // RowKind, so the same binding either toggles expansion (on a
    // category) or enters edit mode (on a text-editable field) —
    // muscle-memory paths for vim users without inventing per-key
    // semantics.
    for ch in ['e', 'o', 'i'] {
        bind(
            reg,
            Some(cursor.clone()),
            no_mods(),
            KeyCodeRepr::Char(ch),
            "tree.activate",
        );
    }
    // `gg` (vim: top) would need a chord state machine the registry
    // doesn't have today. Single-key `G` (Shift+g) for last row is
    // the achievable subset; `Home` / `End` cover the same ground
    // for non-vim users.
    bind(
        reg,
        Some(cursor.clone()),
        shift_only(),
        KeyCodeRepr::Char('G'),
        "tree.last",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::End,
        "tree.last",
    );
    bind(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Home,
        "tree.first",
    );
    bind(
        reg,
        Some(cursor),
        no_mods(),
        KeyCodeRepr::Esc,
        "tree.collapse_or_ascend",
    );
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
        "accounts.compose.open",
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
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('d'),
        "accounts.delete_confirm",
    );
    bind_prefix(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('f'),
        "accounts.fork_provider",
    );
    // h / l (and Left / Right) cycle through selector options when
    // the focused row is a selector field. The command itself
    // checks `RowKind` and no-ops on non-selector rows, so binding
    // at the broad accounts-subtree prefix is fine.
    for (key, id) in [
        (KeyCodeRepr::Char('h'), "cycle.field.prev"),
        (KeyCodeRepr::Left, "cycle.field.prev"),
        (KeyCodeRepr::Char('l'), "cycle.field.next"),
        (KeyCodeRepr::Right, "cycle.field.next"),
    ] {
        bind_prefix(reg, accounts_subtree.clone(), no_mods(), key, id);
    }
    let models_subtree = oxpath!("settings", "models");
    bind_prefix(
        reg,
        models_subtree.clone(),
        shift_only(),
        KeyCodeRepr::Char('P'),
        "models.set_bootstrap",
    );
    // `r` refreshes the focused model's owning account catalog. Useful
    // both when focused on an account row and when focused on a model
    // row (the latter is what the accordion makes natural).
    bind_prefix(
        reg,
        models_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('r'),
        "account.refresh",
    );
    bind_prefix(
        reg,
        models_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('d'),
        "models.toggle_default",
    );
    // `m` opens the manual-model entry form for the focused account.
    // Bound at Prefix(settings/models) so it fires anywhere inside the
    // expanded Models section — the empty-catalog rows are the natural
    // launch point but a focused model row works too.
    bind_prefix(
        reg,
        models_subtree,
        no_mods(),
        KeyCodeRepr::Char('m'),
        "models.add_manual",
    );
}

/// Inline edit-mode bindings under the synthetic cursor
/// `settings/_edit_mode`. While `ui/settings/edit_mode = true` the
/// dispatcher routes through this scope, shadowing tree-nav and
/// per-row keys. Printable chars and Backspace mutate the edit
/// buffer; Enter commits the buffer to the field's data path; Esc
/// cancels without writing.
///
/// Phase classification mirrors the compose form: Esc is a lifecycle
/// key the scope claims at `Phase::Capture` (cancel always wins over
/// any leaf claim); Enter commits at `Phase::Bubble` so a future
/// multi-line text leaf could shadow it with a `Phase::Target`
/// newline-insert binding; printable chars and Backspace stay at
/// `Phase::Target` (they mutate the buffer — the leaf claim).
fn register_edit_mode(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_edit_mode");
    // Printable ASCII (0x20..=0x7E) → edit.insert_char.
    //
    // Modifier handling mirrors the encode/parse round-trip:
    // terminals report uppercase letters as (Char('A'), shift), and
    // parse_key_str sets `shift: true` for any uppercase ASCII letter.
    // Bind ASCII uppercase letters with shift_only() and everything
    // else with no_mods(), so a typed capital reaches edit.insert_char
    // instead of falling through to the input-store path.
    for byte in 0x20u8..=0x7E {
        let ch = byte as char;
        let modifiers = if ch.is_ascii_uppercase() {
            shift_only()
        } else {
            no_mods()
        };
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(scope.clone()),
            mode: None,
            key: KeyChord {
                modifiers,
                code: KeyCodeRepr::Char(ch),
            },
            command_id: cmd("edit.insert_char"),
            phase: Phase::Target,
        });
    }
    bind(
        reg,
        Some(scope.clone()),
        no_mods(),
        KeyCodeRepr::Backspace,
        "edit.delete_back",
    );
    // Enter commits at Bubble: leaves (none today, but a future
    // multi-line text editor at Target) get first crack at Enter.
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(scope.clone()),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Enter,
        },
        command_id: cmd("edit.commit"),
        phase: Phase::Bubble,
    });
    // Esc cancels at Capture: lifecycle key claimed before any leaf.
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(scope),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Esc,
        },
        command_id: cmd("edit.cancel"),
        phase: Phase::Capture,
    });
}

/// Register the compose-new-account mode's bindings across two
/// synthetic scopes: the outer **form** scope and the inner **field**
/// scopes (one per field kind). The dispatcher walks them in three
/// phases (capture → target → bubble) when
/// `ui/settings/new_account/active` is `true`. See
/// `docs/ui_framework/architecture.md` "Hierarchical dispatch" for the
/// model.
///
/// Phase classification is carried by each `BindingEntry`'s `phase`
/// field; the dispatcher's generic walk picks bindings up at the phase
/// they declare.
fn register_compose_new_account(reg: &mut BindingRegistry) {
    register_compose_form(reg);
    register_compose_field_text(reg);
    register_compose_field_selector(reg);
}

/// Outer/container scope for the compose form: lifecycle keys owned by
/// the form regardless of which field is focused. Esc/Tab/Shift+Tab/
/// Up/Down register at `Phase::Capture` so they preempt the focused
/// leaf; Enter registers at `Phase::Bubble` so a future multiline text
/// leaf could shadow it with a `Phase::Target` newline-insert binding.
fn register_compose_form(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_compose_form");

    // Capture phase: lifecycle keys the form claims before the leaf is
    // consulted.
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(scope.clone()),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Esc,
        },
        command_id: cmd("accounts.compose.cancel"),
        phase: Phase::Capture,
    });
    // focus_next: Tab / Down.
    for key in [KeyCodeRepr::Tab, KeyCodeRepr::Down] {
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(scope.clone()),
            mode: None,
            key: KeyChord {
                modifiers: no_mods(),
                code: key,
            },
            command_id: cmd("accounts.compose.focus_next"),
            phase: Phase::Capture,
        });
    }
    // focus_prev: Shift+Tab (terminals emit `BackTab` carrying the
    // canonical `shift` modifier — matches `encode_keychord_to_str` /
    // `parse_key_str` which encode BackTab as the wire string
    // "Shift+Tab" with `shift: true`) / Up.
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(scope.clone()),
        mode: None,
        key: KeyChord {
            modifiers: shift_only(),
            code: KeyCodeRepr::BackTab,
        },
        command_id: cmd("accounts.compose.focus_prev"),
        phase: Phase::Capture,
    });
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(scope.clone()),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Up,
        },
        command_id: cmd("accounts.compose.focus_prev"),
        phase: Phase::Capture,
    });

    // Bubble phase: caught only if the leaf didn't claim Enter at
    // target. (No leaf does today; a future multiline text field
    // could.)
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(scope),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Enter,
        },
        command_id: cmd("accounts.compose.commit"),
        phase: Phase::Bubble,
    });
}

/// Inner/leaf scope for compose form fields of kind `Text` (Name /
/// Endpoint / Key). Target-phase only: printable ASCII goes to
/// insert_char, Backspace pops the focused field's buffer. Uppercase
/// letters bind with `shift_only()` so the encode/parse round-trip
/// lines up with the input store (mirrors `register_edit_mode`).
fn register_compose_field_text(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_compose_field_text");

    for byte in 0x20u8..=0x7E {
        let ch = byte as char;
        let modifiers = if ch.is_ascii_uppercase() {
            shift_only()
        } else {
            no_mods()
        };
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(scope.clone()),
            mode: None,
            key: KeyChord {
                modifiers,
                code: KeyCodeRepr::Char(ch),
            },
            command_id: cmd("accounts.compose.insert_char"),
            phase: Phase::Target,
        });
    }
    bind(
        reg,
        Some(scope),
        no_mods(),
        KeyCodeRepr::Backspace,
        "accounts.compose.delete_back",
    );
}

/// Inner/leaf scope for compose form fields of kind `Selector`
/// (Protocol / Auth). Target-phase only: h / Left cycle back, l /
/// Right cycle forward. Selector fields don't consume typed chars, so
/// no printable-ASCII bindings live here — when the user types `h`
/// while focused on a selector, the dispatcher routes the keystroke
/// through this scope's `Char('h')` binding rather than the text
/// scope's insert_char.
fn register_compose_field_selector(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_compose_field_selector");

    for (key, id) in [
        (KeyCodeRepr::Char('h'), "accounts.compose.cycle_back"),
        (KeyCodeRepr::Left, "accounts.compose.cycle_back"),
        (KeyCodeRepr::Char('l'), "accounts.compose.cycle_forward"),
        (KeyCodeRepr::Right, "accounts.compose.cycle_forward"),
    ] {
        bind(reg, Some(scope.clone()), no_mods(), key, id);
    }
}

/// Register the manual-model entry mode's bindings across the
/// compound widget's scopes. The dispatcher routes to these scopes
/// when `ui/settings/manual_model/stage` holds a typed
/// `ManualModelStage` value (PascalCase wire shape).
///
/// Phase split:
/// - Form scope `settings/_manual_model` claims lifecycle keys:
///   Esc (Capture, cancels the wizard) and Enter (Bubble, advances
///   the stage so a future multi-line stage can claim Enter at Target
///   first).
/// - Per-stage leaf scopes `_manual_model/Id`, `_manual_model/Ctx`,
///   `_manual_model/Out` claim text-input keys: printable ASCII
///   (`insert_char`) and Backspace (`delete_back`) at Target. The
///   command bodies read the active stage from snapshot, so a single
///   command id services all three stages.
fn register_manual_model(reg: &mut BindingRegistry) {
    let form_scope = oxpath!("settings", "_manual_model");

    // Esc — Capture phase: the wizard claims Esc before any leaf, so
    // a future per-stage Esc handler can't shadow lifecycle cancel.
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(form_scope.clone()),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Esc,
        },
        command_id: cmd("models.compose_manual.cancel"),
        phase: Phase::Capture,
    });

    // Enter — Bubble phase: leaf stages get first crack at Enter
    // (Target) so a future multi-line stage can insert a newline; if
    // nothing claims it there, the form advances on Bubble.
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(form_scope),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Enter,
        },
        command_id: cmd("models.compose_manual.commit"),
        phase: Phase::Bubble,
    });

    // Per-stage leaves: printable ASCII + Backspace at Target. Stages
    // share command ids; the commands read the active stage from
    // snapshot and apply per-stage rules (e.g. Ctx/Out digits-only).
    for stage_scope in [
        oxpath!("settings", "_manual_model", "Id"),
        oxpath!("settings", "_manual_model", "Ctx"),
        oxpath!("settings", "_manual_model", "Out"),
    ] {
        for byte in 0x20u8..=0x7E {
            let ch = byte as char;
            let modifiers = if ch.is_ascii_uppercase() {
                shift_only()
            } else {
                no_mods()
            };
            reg.register(BindingEntry {
                screen: Screen::Settings,
                scope: BindingScope::Exact(stage_scope.clone()),
                mode: None,
                key: KeyChord {
                    modifiers,
                    code: KeyCodeRepr::Char(ch),
                },
                command_id: cmd("models.compose_manual.insert_char"),
                phase: Phase::Target,
            });
        }
        bind(
            reg,
            Some(stage_scope),
            no_mods(),
            KeyCodeRepr::Backspace,
            "models.compose_manual.delete_back",
        );
    }
}

/// Register the pending-delete confirmation mode's bindings at the
/// synthetic `settings/_pending_delete` cursor scope. The dispatcher
/// routes to this scope when `ui/settings/pending_delete` is `Some(_)`.
///
/// Phases: y/n are semantic actions on the focused dialog (Target). Esc
/// is a lifecycle key that the container claims before any leaf sees it
/// (Capture) — same shape as compose-Esc.
fn register_pending_delete(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_pending_delete");
    bind(
        reg,
        Some(scope.clone()),
        no_mods(),
        KeyCodeRepr::Char('y'),
        "accounts.confirm.delete",
    );
    bind(
        reg,
        Some(scope.clone()),
        no_mods(),
        KeyCodeRepr::Char('n'),
        "accounts.confirm.cancel",
    );
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Exact(scope),
        mode: None,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Esc,
        },
        command_id: cmd("accounts.confirm.cancel"),
        phase: Phase::Capture,
    });
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
        phase: Phase::Target,
    });
    // Ctrl+S persists the in-memory runtime config to ~/.ox/config.toml.
    // Without this binding `app.save` was registered but unreachable —
    // every edit lived only in the broker's runtime layer and was lost
    // on restart. Anywhere-scoped so save works from any cursor depth.
    reg.register(BindingEntry {
        screen: Screen::Settings,
        scope: BindingScope::Anywhere,
        mode: None,
        key: KeyChord {
            modifiers: ctrl_only(),
            code: KeyCodeRepr::Char('s'),
        },
        command_id: cmd("app.save"),
        phase: Phase::Target,
    });
}

/// Register every day-one settings binding into `reg`.
pub fn register(reg: &mut BindingRegistry) {
    register_global(reg);
    register_edit_mode(reg);
    register_compose_new_account(reg);
    register_manual_model(reg);
    register_pending_delete(reg);
    register_index(reg);
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
                Phase::Target,
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
                Phase::Target,
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
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("tree.collapse_or_ascend"));
    }

    #[test]
    fn accounts_a_resolves_to_accounts_compose_open() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "accounts"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('a')),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("accounts.compose.open"));
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
                Phase::Target,
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
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("account.refresh"));
    }

    #[test]
    fn focused_model_row_p_resolves_to_set_bootstrap() {
        let reg = populated();
        let acct = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        let model = ox_kernel::PathComponent::try_new("claude_haiku").unwrap();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "models", acct, model),
                None,
                &key(shift_only(), KeyCodeRepr::Char('P')),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("models.set_bootstrap"));
    }

    #[test]
    fn edit_mode_printable_char_resolves_to_edit_insert_char() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_edit_mode"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('x')),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("edit.insert_char"));
    }

    #[test]
    fn edit_mode_backspace_resolves_to_edit_delete_back() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_edit_mode"),
                None,
                &key(no_mods(), KeyCodeRepr::Backspace),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("edit.delete_back"));
    }

    #[test]
    fn edit_mode_enter_resolves_to_edit_commit() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_edit_mode"),
                None,
                &key(no_mods(), KeyCodeRepr::Enter),
                Phase::Bubble,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("edit.commit"));
    }

    #[test]
    fn edit_mode_esc_resolves_to_edit_cancel() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_edit_mode"),
                None,
                &key(no_mods(), KeyCodeRepr::Esc),
                Phase::Capture,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("edit.cancel"));
    }

    #[test]
    fn models_capital_p_resolves_to_set_bootstrap() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "models"),
                None,
                &key(shift_only(), KeyCodeRepr::Char('P')),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("models.set_bootstrap"));
    }

    #[test]
    fn models_d_resolves_to_toggle_default() {
        // `d` under settings/models toggles default-available membership.
        // The same key under settings/accounts is bound to
        // accounts.delete_confirm; the prefix scopes are disjoint so
        // resolution is unambiguous.
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "models"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('d')),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("models.toggle_default"));
    }

    #[test]
    fn manual_model_esc_at_form_scope_is_capture_phase() {
        // Esc is registered at the form scope `_manual_model` under
        // Phase::Capture — the wizard claims it before any per-stage
        // leaf sees it.
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_manual_model"),
                None,
                &key(no_mods(), KeyCodeRepr::Esc),
                Phase::Capture,
            )
            .expect("Esc should resolve at Capture");
        assert_eq!(hit, &cmd("models.compose_manual.cancel"));
    }

    #[test]
    fn manual_model_enter_at_form_scope_is_bubble_phase() {
        // Enter advances the wizard from the form scope on Bubble; this
        // leaves Target free for a future multi-line stage to claim
        // Enter as "insert newline" at the leaf.
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_manual_model"),
                None,
                &key(no_mods(), KeyCodeRepr::Enter),
                Phase::Bubble,
            )
            .expect("Enter should resolve at Bubble");
        assert_eq!(hit, &cmd("models.compose_manual.commit"));
    }

    #[test]
    fn manual_model_printable_at_id_leaf_is_target_phase() {
        // Printable ASCII lives on the per-stage leaf scope; same
        // command id services all three stages (the command body reads
        // the active stage from snapshot).
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_manual_model", "Id"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('x')),
                Phase::Target,
            )
            .expect("'x' should resolve at the Id leaf scope");
        assert_eq!(hit, &cmd("models.compose_manual.insert_char"));
    }

    #[test]
    fn manual_model_backspace_at_ctx_leaf_is_target_phase() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_manual_model", "Ctx"),
                None,
                &key(no_mods(), KeyCodeRepr::Backspace),
                Phase::Target,
            )
            .expect("Backspace should resolve at the Ctx leaf scope");
        assert_eq!(hit, &cmd("models.compose_manual.delete_back"));
    }

    #[test]
    fn manual_model_printable_at_out_leaf_is_target_phase() {
        let reg = populated();
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "_manual_model", "Out"),
                None,
                &key(no_mods(), KeyCodeRepr::Char('7')),
                Phase::Target,
            )
            .expect("'7' should resolve at the Out leaf scope");
        assert_eq!(hit, &cmd("models.compose_manual.insert_char"));
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
                .lookup(entry.screen, cursor, entry.mode, &entry.key, entry.phase)
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
        //
        // One known shadow shape is intentional:
        //   - `field.insert`: the inline-edit text helper blankets
        //     printable ASCII, then per-row keys (e.g. `t` → `account.test`)
        //     are registered earlier under the same scope to override.
        //
        // Compose-mode no longer shadows within a single scope: the
        // h / l selector bindings live in `_compose_field_selector`
        // while the printable-ASCII insert_char bindings live in
        // `_compose_field_text`, so they never share a key+scope.
        for (entry, winner) in &shadowed {
            let id = &entry.command_id.0;
            let is_known_text_helper = id == "field.insert";
            assert!(
                is_known_text_helper,
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
