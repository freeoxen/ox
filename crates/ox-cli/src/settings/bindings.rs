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
use ox_types::{BindingEntry, BindingScope, CommandId, KeyChord, Phase};

use crate::settings::BindingRegistry;

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

/// Bind a key at `Phase::Capture` — lifecycle keys claimed by a
/// container before any leaf scope sees them (e.g. Esc cancels a
/// modal regardless of which inner cursor holds focus). The
/// dispatcher walks Capture outer → inner, so a Capture binding at
/// the container scope fires before any Target binding inside.
fn bind_capture(
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
        scope,
        key: KeyChord { modifiers, code },
        command_id: cmd(command_id),
        phase: Phase::Capture,
        priority: 200,
    });
}

/// Bind a key at `Phase::Target` — the leaf-scope binding that
/// claims a key for the currently-focused cursor. Use for text-input
/// keys on a text-editing leaf, action keys on a single-scope widget,
/// and any binding that should fire only when its exact scope holds
/// focus (no inner/outer shadowing semantics needed).
fn bind_target(
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
        scope,
        key: KeyChord { modifiers, code },
        command_id: cmd(command_id),
        phase: Phase::Target,
        priority: 200,
    });
}

/// Bind a key at `Phase::Bubble` for outer-scope default bindings —
/// page-cursor navigation, focused-row prefix actions, whole-screen
/// fallbacks. The dispatcher walks Bubble inner → outer, so an inner
/// compound widget's leaf can claim the same key at `Phase::Target`
/// and shadow these outer defaults.
fn bind_bubble(
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
        scope,
        key: KeyChord { modifiers, code },
        command_id: cmd(command_id),
        phase: Phase::Bubble,
        priority: 200,
    });
}

/// Bind a key under a `Prefix` scope at `Phase::Bubble` — per-row
/// commands that act on whichever subtree row is currently focused.
/// Same Bubble semantics as `bind_bubble`: an inner compound widget's
/// leaf can shadow with a `Phase::Target` binding.
fn bind_prefix_bubble(
    reg: &mut BindingRegistry,
    prefix: Path,
    modifiers: KeyModifierSet,
    code: KeyCodeRepr,
    command_id: &str,
) {
    reg.register(BindingEntry {
        scope: BindingScope::Prefix(prefix),
        key: KeyChord { modifiers, code },
        command_id: cmd(command_id),
        phase: Phase::Bubble,
        priority: 200,
    });
}

// ---------------------------------------------------------------------------
// Priority-aware variants. Same shape as the helpers above but the caller
// chooses where the binding sits in the status-bar curation order. Lower
// `priority` = more important; the bottom bar shows the top-N hints that
// fit available width. Reserve low single-digit / low-tens priorities for
// keys the user reaches for constantly (j/k/Enter); reserve mid-tens
// through low-hundreds for per-page action keys (r/m/P/a/d/t/…).
// ---------------------------------------------------------------------------

fn bind_bubble_with_priority(
    reg: &mut BindingRegistry,
    cursor: Option<Path>,
    modifiers: KeyModifierSet,
    code: KeyCodeRepr,
    command_id: &str,
    priority: u8,
) {
    let scope = match cursor {
        Some(p) => BindingScope::Exact(p),
        None => BindingScope::Anywhere,
    };
    reg.register(BindingEntry {
        scope,
        key: KeyChord { modifiers, code },
        command_id: cmd(command_id),
        phase: Phase::Bubble,
        priority,
    });
}

fn bind_prefix_bubble_with_priority(
    reg: &mut BindingRegistry,
    prefix: Path,
    modifiers: KeyModifierSet,
    code: KeyCodeRepr,
    command_id: &str,
    priority: u8,
) {
    reg.register(BindingEntry {
        scope: BindingScope::Prefix(prefix),
        key: KeyChord { modifiers, code },
        command_id: cmd(command_id),
        phase: Phase::Bubble,
        priority,
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
            scope: BindingScope::Exact(cursor.clone()),
            key: KeyChord {
                modifiers: no_mods(),
                code: KeyCodeRepr::Char(ch),
            },
            command_id: cmd("field.insert"),
            phase: Phase::Target,
            priority: 200,
        });
    }
    // Backspace.
    reg.register(BindingEntry {
        scope: BindingScope::Exact(cursor),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Backspace,
        },
        command_id: cmd("field.delete_back"),
        phase: Phase::Target,
        priority: 200,
    });
}

// ---------------------------------------------------------------------------
// Per-scope registration
// ---------------------------------------------------------------------------

fn register_index(reg: &mut BindingRegistry) {
    // Page-cursor bindings live at `Phase::Bubble`: they're the page's
    // *default* row-navigation handlers, fired only when no inner
    // compound widget (compose / manual-model / edit-mode /
    // pending-delete) claimed the key at Target.
    //
    // Scope is `Exact(settings)` — the common ancestor of every cursor
    // on the settings screen. Under cursor-as-focus the dispatcher's
    // `compute_scope_path` is the cursor's ancestor chain, so the
    // `settings` scope sits at the outer end of every focused cursor's
    // path. Compound widgets register their own Target/Capture
    // bindings on inner scopes, which the Bubble walk reaches first.
    let cursor = oxpath!("settings");
    // Core nav (j/k/Enter) and their arrow aliases get the lowest
    // priorities so they always claim a slot in the status bar.
    bind_bubble_with_priority(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Char('j'),
        "tree.next",
        10,
    );
    bind_bubble_with_priority(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Down,
        "tree.next",
        11,
    );
    bind_bubble_with_priority(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Char('k'),
        "tree.prev",
        10,
    );
    bind_bubble_with_priority(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Up,
        "tree.prev",
        11,
    );
    bind_bubble_with_priority(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Enter,
        "tree.activate",
        15,
    );
    // Vim aliases: `e` (edit), `o` (open), `i` (insert) all route
    // to `tree.activate`. `tree.activate` already dispatches by
    // RowKind, so the same binding either toggles expansion (on a
    // category) or enters edit mode (on a text-editable field) —
    // muscle-memory paths for vim users without inventing per-key
    // semantics.
    for ch in ['e', 'o', 'i'] {
        bind_bubble(
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
    bind_bubble(
        reg,
        Some(cursor.clone()),
        shift_only(),
        KeyCodeRepr::Char('G'),
        "tree.last",
    );
    bind_bubble(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::End,
        "tree.last",
    );
    bind_bubble(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Home,
        "tree.first",
    );
    bind_bubble_with_priority(
        reg,
        Some(cursor.clone()),
        no_mods(),
        KeyCodeRepr::Esc,
        "tree.collapse_or_ascend",
        20,
    );
    // `q` is the unconditional exit-screen escape hatch. Esc has
    // collapse-on-deep-cursor semantics, which is fine for navigating
    // back through expansion levels but inconvenient when the user
    // just wants out. `q` skips the ascend ladder entirely. Prefix
    // scope so it matches from any focus under `settings/*` without
    // depending on the dispatcher's bubble walk reaching the outer
    // `settings` exact scope.
    bind_prefix_bubble_with_priority(
        reg,
        cursor,
        no_mods(),
        KeyCodeRepr::Char('q'),
        "nav.exit_screen",
        25,
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
    // Focused-row prefix bindings declare `Phase::Bubble`: they're the
    // outer page's default actions for whichever row sits inside the
    // subtree, and an inner compound widget's leaf (compose field,
    // manual-model stage, edit-mode buffer) can shadow the same key
    // at `Phase::Target`.
    let accounts_subtree = oxpath!("settings", "accounts");
    bind_prefix_bubble_with_priority(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('a'),
        "accounts.compose.open",
        30,
    );
    bind_prefix_bubble_with_priority(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('t'),
        "account.test",
        40,
    );
    bind_prefix_bubble_with_priority(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('r'),
        "account.refresh",
        35,
    );
    bind_prefix_bubble_with_priority(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('d'),
        "accounts.delete_confirm",
        50,
    );
    bind_prefix_bubble_with_priority(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('f'),
        "accounts.fork_provider",
        60,
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
        bind_prefix_bubble(reg, accounts_subtree.clone(), no_mods(), key, id);
    }
    let models_subtree = oxpath!("settings", "models");
    bind_prefix_bubble_with_priority(
        reg,
        models_subtree.clone(),
        shift_only(),
        KeyCodeRepr::Char('P'),
        "models.set_bootstrap",
        30,
    );
    // `r` refreshes the focused model's owning account catalog. Useful
    // both when focused on an account row and when focused on a model
    // row (the latter is what the accordion makes natural).
    bind_prefix_bubble_with_priority(
        reg,
        models_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('r'),
        "account.refresh",
        35,
    );
    bind_prefix_bubble_with_priority(
        reg,
        models_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('d'),
        "models.toggle_default",
        40,
    );
    // `m` opens the manual-model entry form for the focused account.
    // Bound at Prefix(settings/models) so it fires anywhere inside the
    // expanded Models section — the empty-catalog rows are the natural
    // launch point but a focused model row works too.
    bind_prefix_bubble_with_priority(
        reg,
        models_subtree,
        no_mods(),
        KeyCodeRepr::Char('m'),
        "models.add_manual",
        45,
    );
}

/// Inline edit-mode bindings under the synthetic cursor
/// `settings/_edit`. Under cursor-as-focus, the cursor sitting at
/// `settings/_edit` IS the engaged state — the dispatcher routes
/// through this scope, shadowing tree-nav and per-row keys.
///
/// Lifecycle keys (Backspace/Enter/Esc) stay as discrete bindings so
/// the help screen can enumerate them. Printable input — the bulk of
/// the table — moves to a single opaque `TextInputHandler` registered
/// at the same scope+phase, replacing ~96 discrete `BindingEntry`s
/// (one per printable ASCII char). The discrete tier always beats
/// handlers at the same (scope, phase), so the three lifecycle keys
/// (Backspace, Enter, Esc) keep their lookup paths unchanged.
///
/// Phase classification mirrors the compose form: Esc is claimed at
/// `Phase::Capture` (cancel wins over any leaf); Enter commits at
/// `Phase::Bubble` so a future multi-line text leaf could shadow with
/// a `Phase::Target` newline-insert binding; Backspace and the
/// text-input handler stay at `Phase::Target` (the leaf claim).
fn register_edit_mode(reg: &mut BindingRegistry) {
    use std::sync::Arc;

    use horns_core::HandlerEntry;

    let scope = oxpath!("settings", "_edit");
    bind_target(
        reg,
        Some(scope.clone()),
        no_mods(),
        KeyCodeRepr::Backspace,
        "edit.delete_back",
    );
    // Enter commits at Bubble: leaves (none today, but a future
    // multi-line text editor at Target) get first crack at Enter.
    reg.register(BindingEntry {
        scope: BindingScope::Exact(scope.clone()),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Enter,
        },
        command_id: cmd("edit.commit"),
        phase: Phase::Bubble,
        priority: 200,
    });
    // Esc cancels at Capture: lifecycle key claimed before any leaf.
    reg.register(BindingEntry {
        scope: BindingScope::Exact(scope.clone()),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Esc,
        },
        command_id: cmd("edit.cancel"),
        phase: Phase::Capture,
        priority: 200,
    });
    // Opaque text-input handler: claims any un-modified or shift-only
    // printable char and produces the same buffer write the discrete
    // `edit.insert_char` command used to. Replaces ~96 `BindingEntry`s.
    let text_input = Arc::new(crate::settings::commands::edit::TextInputHandler::new(
        oxpath!("ui", "settings", "edit", "buffer"),
        oxpath!("ui", "settings", "edit", "target_path"),
    ));
    reg.register_handler(HandlerEntry {
        scope: BindingScope::Exact(scope),
        phase: Phase::Target,
        handler: text_input,
    });
}

/// Register the compose-new-account mode's bindings across the form
/// scope and one scope per field. Under cursor-as-focus, the cursor's
/// path encodes both the mode (cursor under `settings/_compose_form`)
/// and the focused sub-element (`settings/_compose_form/<field>`); the
/// dispatcher's scope path walks cursor ancestors so both scopes sit
/// on the active path with the field scope as the leaf.
///
/// Phase classification is carried by each `BindingEntry`'s `phase`
/// field; the dispatcher's generic walk picks bindings up at the phase
/// they declare.
fn register_compose_new_account(reg: &mut BindingRegistry) {
    register_compose_form(reg);
    // Text fields: printable ASCII insert + Backspace delete at Target.
    for field in ["name", "endpoint", "key"] {
        register_compose_text_field(reg, field);
    }
    // Selector fields: h/l/Left/Right cycle at Target.
    for field in ["protocol", "auth"] {
        register_compose_selector_field(reg, field);
    }
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
        scope: BindingScope::Exact(scope.clone()),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Esc,
        },
        command_id: cmd("accounts.compose.cancel"),
        phase: Phase::Capture,
        priority: 200,
    });
    // focus_next: Tab / Down.
    for key in [KeyCodeRepr::Tab, KeyCodeRepr::Down] {
        reg.register(BindingEntry {
            scope: BindingScope::Exact(scope.clone()),
            key: KeyChord {
                modifiers: no_mods(),
                code: key,
            },
            command_id: cmd("accounts.compose.focus_next"),
            phase: Phase::Capture,
            priority: 200,
        });
    }
    // focus_prev: Shift+Tab (terminals emit `BackTab` carrying the
    // canonical `shift` modifier — matches `encode_keychord_to_str` /
    // `parse_key_str` which encode BackTab as the wire string
    // "Shift+Tab" with `shift: true`) / Up.
    reg.register(BindingEntry {
        scope: BindingScope::Exact(scope.clone()),
        key: KeyChord {
            modifiers: shift_only(),
            code: KeyCodeRepr::BackTab,
        },
        command_id: cmd("accounts.compose.focus_prev"),
        phase: Phase::Capture,
        priority: 200,
    });
    reg.register(BindingEntry {
        scope: BindingScope::Exact(scope.clone()),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Up,
        },
        command_id: cmd("accounts.compose.focus_prev"),
        phase: Phase::Capture,
        priority: 200,
    });

    // Bubble phase: caught only if the leaf didn't claim Enter at
    // target. (No leaf does today; a future multiline text field
    // could.)
    reg.register(BindingEntry {
        scope: BindingScope::Exact(scope),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Enter,
        },
        command_id: cmd("accounts.compose.commit"),
        phase: Phase::Bubble,
        priority: 200,
    });
}

/// Per-text-field leaf scope under cursor-as-focus. The cursor sits at
/// `settings/_compose_form/<field>` while typing into a text field, so
/// every text field gets its own leaf scope hosting the same printable
/// ASCII insert + Backspace delete bindings. Uppercase letters bind
/// with `shift_only()` so the encode/parse round-trip lines up with
/// the input store (mirrors `register_edit_mode`).
fn register_compose_text_field(reg: &mut BindingRegistry, field: &str) {
    let comp = ox_kernel::PathComponent::try_new(field)
        .expect("compose text field id must be a valid identifier");
    let scope = oxpath!("settings", "_compose_form", comp);

    for byte in 0x20u8..=0x7E {
        let ch = byte as char;
        let modifiers = if ch.is_ascii_uppercase() {
            shift_only()
        } else {
            no_mods()
        };
        reg.register(BindingEntry {
            scope: BindingScope::Exact(scope.clone()),
            key: KeyChord {
                modifiers,
                code: KeyCodeRepr::Char(ch),
            },
            command_id: cmd("accounts.compose.insert_char"),
            phase: Phase::Target,
            priority: 200,
        });
    }
    bind_target(
        reg,
        Some(scope),
        no_mods(),
        KeyCodeRepr::Backspace,
        "accounts.compose.delete_back",
    );
}

/// Per-selector-field leaf scope under cursor-as-focus. Target-phase
/// only: h / Left cycle back, l / Right cycle forward. Selector fields
/// don't consume typed chars, so no printable-ASCII bindings live here
/// — when the user types `h` while focused on a selector, the
/// dispatcher routes the keystroke through this scope's `Char('h')`
/// binding rather than a text scope's insert_char.
fn register_compose_selector_field(reg: &mut BindingRegistry, field: &str) {
    let comp = ox_kernel::PathComponent::try_new(field)
        .expect("compose selector field id must be a valid identifier");
    let scope = oxpath!("settings", "_compose_form", comp);

    for (key, id) in [
        (KeyCodeRepr::Char('h'), "accounts.compose.cycle_back"),
        (KeyCodeRepr::Left, "accounts.compose.cycle_back"),
        (KeyCodeRepr::Char('l'), "accounts.compose.cycle_forward"),
        (KeyCodeRepr::Right, "accounts.compose.cycle_forward"),
    ] {
        bind_target(reg, Some(scope.clone()), no_mods(), key, id);
    }
}

/// Register the manual-model entry mode's bindings across the
/// compound widget's scopes. Under cursor-as-focus, the cursor's path
/// encodes both the mode (cursor under `settings/_manual_model`) and
/// the focused sub-element (`settings/_manual_model/<stage>`); the
/// dispatcher's scope path walks cursor ancestors so both scopes sit
/// on the active path with the stage scope as the leaf.
///
/// Phase split:
/// - Form scope `settings/_manual_model` claims lifecycle keys:
///   Esc (Capture, cancels the wizard) and Enter (Bubble, advances
///   the stage so a future multi-line stage can claim Enter at Target
///   first).
/// - Per-stage leaf scopes `_manual_model/id`, `_manual_model/ctx`,
///   `_manual_model/out` claim text-input keys: printable ASCII
///   (`insert_char`) and Backspace (`delete_back`) at Target. The
///   command bodies read the active stage from the cursor, so a single
///   command id services all three stages. The lowercase segment
///   names match compose's per-field convention; the dispatcher only
///   cares that the cursor-string and binding-scope-string agree.
fn register_manual_model(reg: &mut BindingRegistry) {
    let form_scope = oxpath!("settings", "_manual_model");

    // Esc — Capture phase: the form claims Esc before any leaf, so
    // a future per-field Esc handler can't shadow lifecycle cancel.
    reg.register(BindingEntry {
        scope: BindingScope::Exact(form_scope.clone()),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Esc,
        },
        command_id: cmd("models.compose_manual.cancel"),
        phase: Phase::Capture,
        priority: 200,
    });

    // Tab / Down — Capture phase: focus_next preempts the focused
    // leaf so a Tab keystroke walks the form instead of being eaten
    // by an insert handler. Mirrors compose-form focus navigation.
    for key in [KeyCodeRepr::Tab, KeyCodeRepr::Down] {
        reg.register(BindingEntry {
            scope: BindingScope::Exact(form_scope.clone()),
            key: KeyChord {
                modifiers: no_mods(),
                code: key,
            },
            command_id: cmd("models.compose_manual.focus_next"),
            phase: Phase::Capture,
            priority: 200,
        });
    }
    // Shift+Tab — terminals emit BackTab with the shift modifier; mirror
    // compose's encoding so encode/parse round-trips line up.
    reg.register(BindingEntry {
        scope: BindingScope::Exact(form_scope.clone()),
        key: KeyChord {
            modifiers: shift_only(),
            code: KeyCodeRepr::BackTab,
        },
        command_id: cmd("models.compose_manual.focus_prev"),
        phase: Phase::Capture,
        priority: 200,
    });
    reg.register(BindingEntry {
        scope: BindingScope::Exact(form_scope.clone()),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Up,
        },
        command_id: cmd("models.compose_manual.focus_prev"),
        phase: Phase::Capture,
        priority: 200,
    });

    // Enter — Bubble phase: leaf fields get first crack at Enter
    // (Target) so a future multi-line field can insert a newline; if
    // nothing claims it there, the form commits on Bubble.
    reg.register(BindingEntry {
        scope: BindingScope::Exact(form_scope),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Enter,
        },
        command_id: cmd("models.compose_manual.commit"),
        phase: Phase::Bubble,
        priority: 200,
    });

    // Per-stage leaves: printable ASCII + Backspace at Target. Stages
    // share command ids; the commands read the active stage from the
    // cursor and apply per-stage rules (e.g. Ctx/Out digits-only).
    for stage_scope in [
        oxpath!("settings", "_manual_model", "id"),
        oxpath!("settings", "_manual_model", "ctx"),
        oxpath!("settings", "_manual_model", "out"),
    ] {
        for byte in 0x20u8..=0x7E {
            let ch = byte as char;
            let modifiers = if ch.is_ascii_uppercase() {
                shift_only()
            } else {
                no_mods()
            };
            reg.register(BindingEntry {
                scope: BindingScope::Exact(stage_scope.clone()),
                key: KeyChord {
                    modifiers,
                    code: KeyCodeRepr::Char(ch),
                },
                command_id: cmd("models.compose_manual.insert_char"),
                phase: Phase::Target,
                priority: 200,
            });
        }
        bind_target(
            reg,
            Some(stage_scope),
            no_mods(),
            KeyCodeRepr::Backspace,
            "models.compose_manual.delete_back",
        );
    }
}

/// Register the confirm-delete mode's bindings at the synthetic
/// `settings/_confirm_delete` cursor scope. Under cursor-as-focus the
/// dispatcher routes to this scope when the cursor sits at
/// `settings/_confirm_delete`; the target account being confirmed lives
/// at the separate data path `ui/settings/pending_delete/target_account`.
///
/// Phases: y/n are semantic actions on the focused dialog (Target). Esc
/// is a lifecycle key that the scope claims before any leaf sees it
/// (Capture) — same shape as compose-Esc.
fn register_pending_delete(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_confirm_delete");
    bind_target(
        reg,
        Some(scope.clone()),
        no_mods(),
        KeyCodeRepr::Char('y'),
        "accounts.confirm.delete",
    );
    bind_target(
        reg,
        Some(scope.clone()),
        no_mods(),
        KeyCodeRepr::Char('n'),
        "accounts.confirm.cancel",
    );
    reg.register(BindingEntry {
        scope: BindingScope::Exact(scope),
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Esc,
        },
        command_id: cmd("accounts.confirm.cancel"),
        phase: Phase::Capture,
        priority: 200,
    });
}

/// Whole-screen `?` toggles the shortcuts modal regardless of cursor
/// depth. `BindingScope::Anywhere` means specific scopes can still
/// shadow it by registering a same-key binding (none do today). Bound
/// at `Phase::Bubble` so an inner widget's leaf can claim `?` at Target
/// (no such leaf today; future help-context leaves could).
fn register_global(reg: &mut BindingRegistry) {
    // `?` opens the shortcuts modal — rendered separately on the right
    // side of the status bar, but flagged here too so the modal also
    // surfaces it as a high-priority entry. Priority 5 wins ahead of
    // every other binding so it never falls out of the bar.
    reg.register(BindingEntry {
        scope: BindingScope::Anywhere,
        key: KeyChord {
            modifiers: no_mods(),
            code: KeyCodeRepr::Char('?'),
        },
        command_id: cmd("modal.toggle_shortcuts"),
        phase: Phase::Bubble,
        priority: 5,
    });
    // Ctrl+S persists the in-memory runtime config to ~/.ox/config.toml.
    // Without this binding `app.save` was registered but unreachable —
    // every edit lived only in the broker's runtime layer and was lost
    // on restart. Anywhere-scoped so save works from any cursor depth.
    // Bubble-phase: a future text-leaf that wants to capture Ctrl+S for
    // some leaf-local semantic could shadow at Target.
    reg.register(BindingEntry {
        scope: BindingScope::Anywhere,
        key: KeyChord {
            modifiers: ctrl_only(),
            code: KeyCodeRepr::Char('s'),
        },
        command_id: cmd("app.save"),
        phase: Phase::Bubble,
        priority: 200,
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
        // Page-cursor `j` declares `Phase::Bubble` — it's an outer-scope
        // default that fires only when no inner widget claims the key.
        // Scope is `Exact(settings)` — the common ancestor of every
        // cursor on the settings screen — so the dispatcher's Bubble
        // walk reaches it from any focused-row cursor.
        let reg = populated();
        let hit = reg
            .lookup(
                &oxpath!("settings"),
                &key(no_mods(), KeyCodeRepr::Char('j')),
                Phase::Bubble,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("tree.next"));
    }

    #[test]
    fn index_enter_resolves_to_tree_activate() {
        let reg = populated();
        let hit = reg
            .lookup(
                &oxpath!("settings"),
                &key(no_mods(), KeyCodeRepr::Enter),
                Phase::Bubble,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("tree.activate"));
    }

    #[test]
    fn index_esc_resolves_to_tree_collapse_or_ascend() {
        let reg = populated();
        let hit = reg
            .lookup(
                &oxpath!("settings"),
                &key(no_mods(), KeyCodeRepr::Esc),
                Phase::Bubble,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("tree.collapse_or_ascend"));
    }

    #[test]
    fn q_at_any_settings_focus_resolves_to_nav_exit_screen() {
        // `q` is the unconditional exit-screen escape hatch. It must
        // fire from any focus under `settings/*` — page level, account
        // row, account field, compose form, edit buffer — so the user
        // can always get out without walking the ascend ladder.
        let reg = populated();
        for scope in [
            oxpath!("settings"),
            oxpath!("settings", "accounts"),
            oxpath!("settings", "accounts", "alpha"),
            oxpath!("settings", "accounts", "alpha", "endpoint"),
            oxpath!("settings", "_compose_form", "name"),
        ] {
            let hit = reg
                .lookup(
                    &scope,
                    &key(no_mods(), KeyCodeRepr::Char('q')),
                    Phase::Bubble,
                )
                .unwrap_or_else(|| panic!("q must resolve at {scope:?}"));
            assert_eq!(hit, &cmd("nav.exit_screen"));
        }
    }

    #[test]
    fn accounts_a_resolves_to_accounts_compose_open() {
        // Focused-row prefix bindings declare `Phase::Bubble`.
        let reg = populated();
        let hit = reg
            .lookup(
                &oxpath!("settings", "accounts"),
                &key(no_mods(), KeyCodeRepr::Char('a')),
                Phase::Bubble,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("accounts.compose.open"));
    }

    #[test]
    fn focused_account_row_t_resolves_to_account_test() {
        // Per-row prefix binding: `t` fires whenever the cursor sits
        // under `settings/accounts`, including the account leaf row
        // — no page-flip required. Bubble phase — outer-scope default.
        let reg = populated();
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        let hit = reg
            .lookup(
                &oxpath!("settings", "accounts", comp),
                &key(no_mods(), KeyCodeRepr::Char('t')),
                Phase::Bubble,
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
                &oxpath!("settings", "accounts", comp),
                &key(no_mods(), KeyCodeRepr::Char('r')),
                Phase::Bubble,
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
                &oxpath!("settings", "models", acct, model),
                &key(shift_only(), KeyCodeRepr::Char('P')),
                Phase::Bubble,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("models.set_bootstrap"));
    }

    #[test]
    fn edit_mode_printable_char_is_handled_by_text_input_handler() {
        // Printable input under `_edit` no longer routes through the
        // discrete tier — the 96 per-char BindingEntries have been
        // replaced by a single `TextInputHandler` at Target. The
        // discrete lookup must miss, and the handler lookup must hit.
        let reg = populated();
        let cursor = oxpath!("settings", "_edit");
        let chord = key(no_mods(), KeyCodeRepr::Char('x'));

        assert!(
            reg.lookup(&cursor, &chord, Phase::Target).is_none(),
            "discrete tier must not claim printable chars under _edit anymore",
        );
        assert!(
            reg.lookup_handler(&cursor, &chord, Phase::Target).is_some(),
            "the opaque TextInputHandler should be registered at (_edit, Target)",
        );
    }

    #[test]
    fn edit_mode_backspace_resolves_to_edit_delete_back() {
        let reg = populated();
        let hit = reg
            .lookup(
                &oxpath!("settings", "_edit"),
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
                &oxpath!("settings", "_edit"),
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
                &oxpath!("settings", "_edit"),
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
                &oxpath!("settings", "models"),
                &key(shift_only(), KeyCodeRepr::Char('P')),
                Phase::Bubble,
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
                &oxpath!("settings", "models"),
                &key(no_mods(), KeyCodeRepr::Char('d')),
                Phase::Bubble,
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
                &oxpath!("settings", "_manual_model"),
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
                &oxpath!("settings", "_manual_model"),
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
        // the active stage from the cursor).
        let reg = populated();
        let hit = reg
            .lookup(
                &oxpath!("settings", "_manual_model", "id"),
                &key(no_mods(), KeyCodeRepr::Char('x')),
                Phase::Target,
            )
            .expect("'x' should resolve at the id leaf scope");
        assert_eq!(hit, &cmd("models.compose_manual.insert_char"));
    }

    #[test]
    fn manual_model_backspace_at_ctx_leaf_is_target_phase() {
        let reg = populated();
        let hit = reg
            .lookup(
                &oxpath!("settings", "_manual_model", "ctx"),
                &key(no_mods(), KeyCodeRepr::Backspace),
                Phase::Target,
            )
            .expect("Backspace should resolve at the ctx leaf scope");
        assert_eq!(hit, &cmd("models.compose_manual.delete_back"));
    }

    #[test]
    fn manual_model_printable_at_out_leaf_is_target_phase() {
        let reg = populated();
        let hit = reg
            .lookup(
                &oxpath!("settings", "_manual_model", "out"),
                &key(no_mods(), KeyCodeRepr::Char('7')),
                Phase::Target,
            )
            .expect("'7' should resolve at the out leaf scope");
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
                .lookup(cursor, &entry.key, entry.phase)
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
        // h / l selector bindings live on each selector field's scope
        // (`_compose_form/{protocol,auth}`) while the printable-ASCII
        // insert_char bindings live on each text field's scope
        // (`_compose_form/{name,endpoint,key}`), so they never share a
        // key+scope.
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

    // -----------------------------------------------------------------------
    // Cross-cutting binding-phase invariants
    //
    // These tests scan the production registry built by `register()` and
    // pin phase choices that previously lived only in code-review folklore
    // and per-widget tests. Drift surfaces here at the boundary, with a
    // failure message that names the offending entry — so a future
    // contributor learns the rule without re-reading the dispatcher docs.
    //
    // Compound-widget scope classification (path-prefix predicates):
    //
    //   Compound-widget scope (general):
    //     `BindingScope::Exact(p)` where `p` begins
    //     `settings/_…` — the convention for synthetic
    //     compound-widget cursors. Concretely:
    //       _compose_form, _compose_form/{name,protocol,endpoint,auth,key},
    //       _manual_model, _manual_model/{id,ctx,out},
    //       _confirm_delete, _edit.
    //
    //   Container scope (has a separate leaf scope below it):
    //     _compose_form, _manual_model.
    //
    //   Leaf scope (the focused inner target):
    //     _compose_form/{name,protocol,endpoint,auth,key} — per-field
    //       children of the compose form.
    //     _manual_model/{id,ctx,out} — per-stage children.
    //     _confirm_delete, _edit — single-scope widgets: the scope
    //     IS the leaf when active (no separate form+leaf split). For
    //     these, lifecycle keys (Esc/Enter/Tab/BackTab/Up/Down) are still
    //     hosted on the same scope but at Capture/Bubble, so the
    //     leaf-Target invariant deliberately excludes lifecycle keys.
    // -----------------------------------------------------------------------

    /// True iff the path's first two components are `settings/_*` —
    /// the synthetic compound-widget cursor convention.
    fn is_compound_widget_scope(p: &Path) -> bool {
        p.components.len() >= 2 && p.components[0] == "settings" && p.components[1].starts_with('_')
    }

    /// True iff `p` is the container scope for a widget that has a
    /// separate leaf scope below it (compose form / manual-model form).
    fn is_container_scope(p: &Path) -> bool {
        // Both containers have exactly two components: settings/_name.
        // The single-scope widgets (_confirm_delete, _edit) also
        // have shape settings/_name but no leaf below — they're handled
        // by the leaf classifier and explicitly excluded here.
        if p.components.len() != 2 || p.components[0] != "settings" {
            return false;
        }
        matches!(p.components[1].as_str(), "_compose_form" | "_manual_model")
    }

    /// True iff `p` is a leaf scope of a compound widget.
    ///
    /// Two shapes:
    /// - children of a form-and-leaf widget (`_compose_form/<field>`,
    ///   `_manual_model/id|ctx|out`);
    /// - single-scope widgets where the same scope hosts both lifecycle
    ///   and leaf bindings (`_confirm_delete`, `_edit`).
    fn is_leaf_scope(p: &Path) -> bool {
        if p.components.len() < 2 || p.components[0] != "settings" {
            return false;
        }
        let head = p.components[1].as_str();
        // Split-widget leaves.
        if head == "_compose_form" && p.components.len() == 3 {
            return matches!(
                p.components[2].as_str(),
                "name" | "protocol" | "endpoint" | "auth" | "key"
            );
        }
        if head == "_manual_model" && p.components.len() == 3 {
            return matches!(p.components[2].as_str(), "id" | "ctx" | "out");
        }
        // Single-scope widgets: scope IS the leaf.
        if p.components.len() == 2 {
            return matches!(head, "_confirm_delete" | "_edit");
        }
        false
    }

    /// Keys that compound widgets route as container lifecycle (not
    /// leaf semantics) even when the leaf and the container share a
    /// single scope. Excluded from the "every leaf binding is Target"
    /// scan because on single-scope widgets these are deliberately
    /// hosted at Capture/Bubble on the same scope.
    fn is_lifecycle_key(code: &KeyCodeRepr) -> bool {
        matches!(
            code,
            KeyCodeRepr::Esc
                | KeyCodeRepr::Enter
                | KeyCodeRepr::Tab
                | KeyCodeRepr::BackTab
                | KeyCodeRepr::Up
                | KeyCodeRepr::Down
        )
    }

    fn format_offenders(offenders: &[&BindingEntry]) -> String {
        offenders
            .iter()
            .map(|e| {
                format!(
                    "  scope={:?} key={:?} command_id={:?} phase={:?}",
                    e.scope, e.key, e.command_id, e.phase
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_esc_on_compound_widget_scope_is_capture_phase() {
        // Esc on a compound-widget scope is always a lifecycle cancel
        // that the container claims before any leaf — so it must fire
        // at Capture. Drift would let a leaf swallow Esc and trap the
        // user inside a sub-mode.
        let reg = populated();
        let offenders: Vec<&BindingEntry> = reg
            .entries()
            .iter()
            .filter(|e| matches!(&e.scope, BindingScope::Exact(p) if is_compound_widget_scope(p)))
            .filter(|e| e.key.code == KeyCodeRepr::Esc)
            .filter(|e| e.phase != Phase::Capture)
            .collect();

        assert!(
            offenders.is_empty(),
            "Esc bindings on compound-widget scopes must be Phase::Capture but these aren't:\n{}",
            format_offenders(&offenders),
        );
    }

    #[test]
    fn every_enter_on_compound_widget_container_scope_is_bubble_phase() {
        // Enter on a container scope is the form's commit fallback —
        // it must fire only when no leaf claimed Enter at Target. So
        // every container Enter is Bubble; a future multi-line leaf
        // gets to insert a newline at Target without being shadowed.
        let reg = populated();
        let offenders: Vec<&BindingEntry> = reg
            .entries()
            .iter()
            .filter(|e| matches!(&e.scope, BindingScope::Exact(p) if is_container_scope(p)))
            .filter(|e| e.key.code == KeyCodeRepr::Enter)
            .filter(|e| e.phase != Phase::Bubble)
            .collect();

        assert!(
            offenders.is_empty(),
            "Enter bindings on compound-widget container scopes must be Phase::Bubble but these aren't:\n{}",
            format_offenders(&offenders),
        );
    }

    #[test]
    fn every_binding_on_compound_widget_leaf_scope_is_target_phase() {
        // The leaf scope is where the focused inner widget claims keys
        // — Target phase by definition. Lifecycle keys (Esc/Enter/Tab/
        // BackTab/Up/Down) are excluded: on single-scope widgets like
        // _confirm_delete / _edit the same scope hosts both
        // lifecycle (Capture/Bubble) and leaf (Target) bindings, and
        // the lifecycle ones are pinned by the Esc-Capture and
        // container-Enter-Bubble invariants above.
        let reg = populated();
        let offenders: Vec<&BindingEntry> = reg
            .entries()
            .iter()
            .filter(|e| matches!(&e.scope, BindingScope::Exact(p) if is_leaf_scope(p)))
            .filter(|e| !is_lifecycle_key(&e.key.code))
            .filter(|e| e.phase != Phase::Target)
            .collect();

        assert!(
            offenders.is_empty(),
            "Non-lifecycle bindings on compound-widget leaf scopes must be Phase::Target but these aren't:\n{}",
            format_offenders(&offenders),
        );
    }

    #[test]
    fn every_anywhere_binding_is_bubble_phase() {
        // `BindingScope::Anywhere` is the lowest-specificity scope —
        // it's the screen-wide fallback. Anywhere bindings must fire
        // at Bubble so any inner compound widget's leaf can shadow
        // the same key at Target. A non-Bubble Anywhere would trap
        // the key globally and break the hierarchical-dispatch model.
        let reg = populated();
        let offenders: Vec<&BindingEntry> = reg
            .entries()
            .iter()
            .filter(|e| matches!(&e.scope, BindingScope::Anywhere))
            .filter(|e| e.phase != Phase::Bubble)
            .collect();

        assert!(
            offenders.is_empty(),
            "Bindings at BindingScope::Anywhere must be Phase::Bubble but these aren't:\n{}",
            format_offenders(&offenders),
        );
    }

    #[test]
    fn settings_edit_scope_has_no_more_than_lifecycle_discrete_bindings() {
        // The migration to `TextInputHandler` replaced ~96 per-char
        // `BindingEntry`s under `Exact(settings/_edit)` with a single
        // opaque handler. Only the lifecycle keys (Backspace, Enter,
        // Esc) remain as discrete bindings so the help screen can
        // enumerate them. If a future change re-adds printable-char
        // bindings under this scope, that's almost certainly a
        // regression — the opaque tier exists exactly so the discrete
        // tier stays small.
        let reg = populated();
        let edit_scope = oxpath!("settings", "_edit");
        let edit_count = reg
            .entries()
            .iter()
            .filter(|e| matches!(&e.scope, BindingScope::Exact(p) if p == &edit_scope))
            .count();
        assert_eq!(
            edit_count, 3,
            "expected exactly 3 discrete bindings under settings/_edit (Backspace, Enter, Esc); got {edit_count}",
        );
    }

    #[test]
    fn settings_edit_scope_registers_text_input_handler() {
        // The opaque half of the migration: a single handler at
        // (Exact(_edit), Target) replaces the printable-ASCII bindings.
        let reg = populated();
        let edit_scope = oxpath!("settings", "_edit");
        let handler_count = reg
            .handlers()
            .iter()
            .filter(|h| matches!(&h.scope, BindingScope::Exact(p) if p == &edit_scope))
            .filter(|h| h.phase == Phase::Target)
            .count();
        assert_eq!(
            handler_count, 1,
            "expected exactly one handler at (Exact(settings/_edit), Target)",
        );
    }
}
