# Phase 3: Lift inline-create into `new_account/buffer` mode state — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop using the synthetic `RowKind::AccountAdd` ghost row + `edit_mode` machinery for the inline create flow. Replace with a `ui/settings/new_account/buffer: Option<String>` mode-state path. The renderer reads the buffer and decorates the accounts section accordingly; the dispatcher routes keys through new compose-mode commands at a `settings/_compose_new_account` synthetic binding scope (mirroring the existing `_edit_mode` pattern); `accounts.add` becomes "write `Some("")` to the buffer."

**Architecture:** Two-commit landing. **Commit A** introduces the new infrastructure (compose-mode commands + bindings + dispatcher pass + renderer reading the buffer for affordance decoration) — all dormant because nothing writes the buffer yet. **Commit B** switches: `accounts.add` writes the buffer, `RowKind::AccountAdd` is dropped from `visible_rows`, the dead code (the `RowKind::AccountAdd` arms in `tree::activate`, `edit::commit`, `decorate_row_label`, `edit::insert_char`, plus `begin_account_add` and the AccountAdd-specific tests) is removed. Between A and B the workspace is green and user-visible behavior is unchanged. The renderer's affordance composition + selection-index recompute is the trickiest piece.

**Tech Stack:** Rust workspace; `ox-cli` (commands, bindings, dispatcher, renderer, visible_rows, tests).

**Spec:** [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md) §5 Phase 3 + §4.4 mode-aware dispatch.

---

## File Map

**Modify:**
- `crates/ox-cli/src/settings/commands/mod.rs` — register the four new compose-mode commands.
- `crates/ox-cli/src/settings/commands/account_model.rs` — add the four `accounts.compose.*` commands; rewrite `accounts.add` (Commit B); update its test.
- `crates/ox-cli/src/settings/bindings.rs` — register printable + Backspace + Enter + Esc bindings at the `_compose_new_account` synthetic scope.
- `crates/ox-cli/src/settings/dispatch.rs` — add the compose-mode pass before the existing edit-mode pass.
- `crates/ox-cli/src/settings/renderers/index.rs` — read `new_account/buffer` and prepend an affordance `ListItem { focus: None, … }` after the expanded Accounts header. Drop the `RowKind::AccountAdd` decoration arm in `decorate_row_label` (Commit B).
- `crates/ox-cli/src/settings/visible_rows.rs` — drop `RowKind::AccountAdd` variant + the synthetic ghost-row push in `append_account_rows` + the related tests (Commit B).
- `crates/ox-cli/src/settings/commands/tree.rs` — drop the `RowKind::AccountAdd` arm in `activate` (Commit B).
- `crates/ox-cli/src/settings/commands/edit.rs` — drop the `RowKind::AccountAdd` arm in `commit`, drop the `RowKind::AccountAdd` accept rule in `insert_char`, drop the `begin_account_add` helper, drop the AccountAdd-specific tests (Commit B).

**Create:**
- (none — all additions land in existing files)

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section comments in code. Doc comments explaining WHY are fine.

---

## Task 1: Commit A — add compose-mode infrastructure (dormant)

This task adds all the new pieces but doesn't activate them. Nothing writes `new_account/buffer` yet, so the new dispatcher pass never fires, the new commands are never invoked, and the renderer's buffer-read returns `None` (no affordance decoration emitted). User-visible behavior is unchanged.

The infrastructure being added:

1. Four new commands `accounts.compose.{insert_char, delete_back, commit, cancel}`.
2. Bindings at the `settings/_compose_new_account` synthetic scope.
3. A new dispatcher pass (before the existing edit-mode pass) that consults `new_account/buffer` and looks up bindings at the compose-mode scope when the buffer is `Some`.
4. Renderer logic that reads the buffer and prepends an affordance `ListItem` after the expanded Accounts header.

### Sub-task 1.1: Add the four compose-mode commands

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs` — add four new `command!` blocks plus their helper functions.

The commands mirror the existing `edit.{insert_char, delete_back, commit, cancel}` shapes but read/write `ui/settings/new_account/buffer` instead of `edit_buffer`/`edit_field_path`/`edit_mode`.

- [ ] **Step 1: Add the four `command!` blocks**

In `crates/ox-cli/src/settings/commands/account_model.rs`, after the existing `AccountsAdd` command block, add:

```rust
command! {
    struct_name: AccountsComposeInsertChar,
    id: "accounts.compose.insert_char",
    title: "Insert character",
    description: "Append the just-pressed printable char to the new-account name buffer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, ctx| accounts_compose_insert_char(snap, ctx),
}

command! {
    struct_name: AccountsComposeDeleteBack,
    id: "accounts.compose.delete_back",
    title: "Backspace",
    description: "Pop the last character from the new-account name buffer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_delete_back(snap),
}

command! {
    struct_name: AccountsComposeCommit,
    id: "accounts.compose.commit",
    title: "Create connection",
    description: "Validate the buffered name and materialize the AccountConfig.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_commit(snap),
}

command! {
    struct_name: AccountsComposeCancel,
    id: "accounts.compose.cancel",
    title: "Cancel new connection",
    description: "Discard the new-account buffer; exit compose mode.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "new_account", "buffer"),
        record: Record::parsed(Value::Null),
    }],
}
```

- [ ] **Step 2: Add the helper functions**

After the command blocks, add:

```rust
fn accounts_compose_insert_char(
    data: &mut dyn Reader,
    ctx: &super::command_registry::CommandCtx<'_>,
) -> Vec<Write> {
    use ox_types::key_chord::KeyCodeRepr;
    let chord = match ctx.last_keystroke.as_ref() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let ch = match chord.code {
        KeyCodeRepr::Char(c) => c,
        _ => return Vec::new(),
    };
    let current: String = read_typed(
        data,
        &oxpath!("ui", "settings", "new_account", "buffer"),
    )
    .unwrap_or_default();
    let mut next = current;
    next.push(ch);
    vec![Write {
        path: oxpath!("ui", "settings", "new_account", "buffer"),
        record: Record::parsed(Value::String(next)),
    }]
}

fn accounts_compose_delete_back(data: &mut dyn Reader) -> Vec<Write> {
    let mut current: String = read_typed(
        data,
        &oxpath!("ui", "settings", "new_account", "buffer"),
    )
    .unwrap_or_default();
    if current.pop().is_none() {
        return Vec::new();
    }
    vec![Write {
        path: oxpath!("ui", "settings", "new_account", "buffer"),
        record: Record::parsed(Value::String(current)),
    }]
}

fn accounts_compose_commit(data: &mut dyn Reader) -> Vec<Write> {
    use ox_gate::AccountConfig;
    use ox_kernel::PathComponent;

    let buffer: String = read_typed(
        data,
        &oxpath!("ui", "settings", "new_account", "buffer"),
    )
    .unwrap_or_default();
    let trimmed = buffer.trim();
    // Empty/whitespace: silent no-op so compose mode stays open.
    if trimmed.is_empty() {
        return Vec::new();
    }
    // `_`-prefix: kept transitionally; Phase 7 retires this rule once
    // there are no remaining sentinel paths to collide with.
    if trimmed.starts_with('_') {
        return vec![banner_error(format!(
            "Account name '{}' starts with '_', which is reserved. Try a name without the leading underscore.",
            trimmed
        ))];
    }
    let comp = match PathComponent::try_new(trimmed.to_string()) {
        Ok(c) => c,
        Err(_) => {
            return vec![banner_error(format!(
                "Invalid account name: '{}'",
                trimmed
            ))];
        }
    };

    let cfg = AccountConfig {
        provider: "anthropic".to_string(),
    };
    let new_account_row = oxpath!("settings", "accounts", comp.clone());
    let mut expanded: Vec<String> = read_typed(
        data,
        &oxpath!("ui", "settings", "expanded"),
    )
    .unwrap_or_default();
    let accounts_key = "settings/accounts".to_string();
    let new_row_key = format!("settings/accounts/{}", trimmed);
    if !expanded.iter().any(|s| s == &accounts_key) {
        expanded.push(accounts_key);
    }
    if !expanded.iter().any(|s| s == &new_row_key) {
        expanded.push(new_row_key);
    }

    let acct_path = oxpath!("config", "gate", "accounts", comp);
    let cfg_value = match to_value(&cfg) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let selected_value = match to_value(&Some(trimmed.to_string())) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let expanded_value = match to_value(&expanded) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    vec![
        Write {
            path: acct_path,
            record: Record::parsed(cfg_value),
        },
        Write {
            path: oxpath!("ui", "settings", "accounts", "selected"),
            record: Record::parsed(selected_value),
        },
        Write {
            path: oxpath!("ui", "settings", "cursor"),
            record: Record::parsed(path_to_value(&oxpath!("settings", "index"))),
        },
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&new_account_row)),
        },
        Write {
            path: oxpath!("ui", "settings", "expanded"),
            record: Record::parsed(expanded_value),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "buffer"),
            record: Record::parsed(Value::Null),
        },
    ]
}

fn banner_error(message: String) -> Write {
    use ox_types::settings::GlobalBanner;
    let banner = GlobalBanner::Error {
        message,
        set_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    };
    Write {
        path: oxpath!("ui", "global", "banner"),
        record: Record::parsed(to_value(&banner).unwrap()),
    }
}
```

(Note: `read_typed` is the existing helper imported elsewhere in the file. `path_to_value` is `super::navigation::path_to_value` — already in scope. If a `banner_error` already exists in this file with a different signature, reuse the existing one and don't duplicate. If the existing one matches this shape, drop the duplicate definition.)

- [ ] **Step 3: Register the commands**

In `crates/ox-cli/src/settings/commands/account_model.rs`'s `register` function (where `AccountsAdd::new()` and friends are registered), add:

```rust
reg.register(Box::new(AccountsComposeInsertChar::new()));
reg.register(Box::new(AccountsComposeDeleteBack::new()));
reg.register(Box::new(AccountsComposeCommit::new()));
reg.register(Box::new(AccountsComposeCancel::new()));
```

- [ ] **Step 4: Build**

```
cargo build -p ox-cli
```

Expected: PASS.

### Sub-task 1.2: Add bindings at the `_compose_new_account` synthetic scope

**File:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

Mirror the pattern used by `register_text_editing` (which registers ~96 printable-ASCII + Backspace bindings at the `_edit_mode` synthetic scope).

- [ ] **Step 1: Add `register_compose_new_account` function**

In `crates/ox-cli/src/settings/bindings.rs`, near the existing `register_text_editing` (or wherever similar registration helpers live), add:

```rust
/// Register the compose-new-account mode's bindings at the synthetic
/// `settings/_compose_new_account` cursor scope. The dispatcher routes
/// to this scope when `ui/settings/new_account/buffer` is `Some(_)`.
fn register_compose_new_account(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_compose_new_account");

    // Printable ASCII (0x20..=0x7E) → accounts.compose.insert_char.
    // Mirrors register_text_editing's modifier handling: ASCII
    // uppercase letters bind with shift_only(); everything else with
    // no_mods().
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
        });
    }

    bind(reg, Some(scope.clone()), no_mods(), KeyCodeRepr::Backspace, "accounts.compose.delete_back");
    bind(reg, Some(scope.clone()), no_mods(), KeyCodeRepr::Enter, "accounts.compose.commit");
    bind(reg, Some(scope), no_mods(), KeyCodeRepr::Esc, "accounts.compose.cancel");
}
```

(Match the existing helper signatures — `bind`, `cmd`, `shift_only`, `no_mods`, `BindingEntry`, `BindingScope::Exact` — used by `register_text_editing`. If any are spelled differently in this file, use the local spelling.)

- [ ] **Step 2: Call it from the top-level register**

Find the function that calls `register_text_editing(reg)` (probably the file's top-level `register` function) and add `register_compose_new_account(reg);` next to it. Order doesn't matter — bindings at distinct scopes don't conflict.

- [ ] **Step 3: Build**

```
cargo build -p ox-cli
```

Expected: PASS.

### Sub-task 1.3: Add the dispatcher's compose-mode pass

**File:**
- Modify: `crates/ox-cli/src/settings/dispatch.rs`

Insert a new pass BEFORE the existing edit-mode pass: read `new_account/buffer`; if `Some(_)`, look up bindings at the `_compose_new_account` scope.

- [ ] **Step 1: Add the buffer-reader helper**

In `crates/ox-cli/src/settings/dispatch.rs`, add a function near `read_edit_mode`:

```rust
/// Read `ui/settings/new_account/buffer`. Returns `Some(_)` when the
/// user is composing a new account name (compose-mode active).
fn read_compose_buffer(snapshot: &mut dyn Reader) -> Option<String> {
    use ox_path::oxpath;
    use structfs_core_store::Value;
    let record = snapshot
        .read(&oxpath!("ui", "settings", "new_account", "buffer"))
        .ok()
        .flatten()?;
    match record.as_value()? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}
```

- [ ] **Step 2: Add the compose-mode pass**

In `dispatch_settings_key`, the binding-lookup chain currently looks like:

```rust
let cmd_id = if edit_mode_active {
    bindings.lookup(screen, &edit_scope, mode, key)
} else {
    None
}
.or_else(|| { /* focused-row scope */ })
.or_else(|| { /* page cursor scope */ });
```

Replace with a four-pass chain that consults compose-mode FIRST:

```rust
let compose_active = read_compose_buffer(snapshot).is_some();
let compose_scope = ox_path::oxpath!("settings", "_compose_new_account");
let edit_mode_active = read_edit_mode(snapshot);
let edit_scope = ox_path::oxpath!("settings", "_edit_mode");

let cmd_id = if compose_active {
    bindings.lookup(screen, &compose_scope, mode, key)
} else {
    None
}
.or_else(|| {
    if edit_mode_active {
        bindings.lookup(screen, &edit_scope, mode, key)
    } else {
        None
    }
})
.or_else(|| {
    read_focused(snapshot)
        .as_ref()
        .and_then(|focus| bindings.lookup(screen, focus, mode, key))
})
.or_else(|| bindings.lookup(screen, cursor, mode, key));
```

Update the doc comment above the chain to list four passes (1. compose mode, 2. edit mode, 3. focused-row scope, 4. page cursor) and explain the priority order: compose and edit are mutually exclusive (the spec's mutual-exclusion invariant), but compose takes priority so a stale `edit_mode = true` flag wouldn't shadow a legitimate compose. After Phase 7's cleanup, the mutual-exclusion check could be made stricter; for now, deterministic priority order is sufficient.

- [ ] **Step 3: Add a unit test for the compose-mode pass**

In dispatch.rs's `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn compose_mode_routes_to_compose_scope_when_buffer_is_some() {
    let mut cmds = CommandRegistry::new();
    cmds.register(Box::new(WriteSentinel::new()));

    let mut bindings = BindingRegistry::new();
    bindings.register(BindingEntry {
        screen: Screen::Settings,
        scope: ox_types::BindingScope::Exact(oxpath!("settings", "_compose_new_account")),
        mode: None,
        key: key_char('a'),
        command_id: cmd_id("test.sentinel"),
    });

    let renderers = RendererRegistry::new();
    let mut reader = LocalConfig::default();
    // Seed the buffer to put the dispatcher in compose mode.
    reader
        .write(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Record::parsed(Value::String(String::new())),
        )
        .unwrap();

    let writes = dispatch_settings_key(
        &mut reader,
        Screen::Settings,
        &oxpath!("settings", "index"),
        None,
        &key_char('a'),
        &cmds,
        &bindings,
        &renderers,
    );

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
}

#[test]
fn compose_mode_falls_through_when_buffer_is_absent() {
    let mut cmds = CommandRegistry::new();
    cmds.register(Box::new(WriteSentinel::new()));

    let mut bindings = BindingRegistry::new();
    // Bind ONLY at the compose scope — should not match because
    // buffer is unset.
    bindings.register(BindingEntry {
        screen: Screen::Settings,
        scope: ox_types::BindingScope::Exact(oxpath!("settings", "_compose_new_account")),
        mode: None,
        key: key_char('a'),
        command_id: cmd_id("test.sentinel"),
    });

    let renderers = RendererRegistry::new();
    let mut reader = LocalConfig::default();

    let writes = dispatch_settings_key(
        &mut reader,
        Screen::Settings,
        &oxpath!("settings", "index"),
        None,
        &key_char('a'),
        &cmds,
        &bindings,
        &renderers,
    );

    assert!(writes.is_empty());
}
```

- [ ] **Step 4: Run dispatch + ox-cli lib tests**

```
cargo test -p ox-cli --lib settings::dispatch::tests
cargo test -p ox-cli --lib
```

Expected: PASS.

### Sub-task 1.4: Renderer reads the buffer and emits affordance decoration

**File:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs`

The renderer iterates `visible_rows::enumerate(...)` and maps each row to a `ListItem`. After this sub-task, when the Accounts entry is expanded, the renderer also prepends an affordance ListItem reading the buffer — `Some(buf)` produces an inline name prompt; `None` produces a static "+ New connection" line.

For Commit A, this code path coexists with the existing `RowKind::AccountAdd` synthetic row. Since `new_account/buffer` is never written in Commit A, the affordance ListItem is always the static-affordance variant — and visually it sits next to the existing synthetic ghost row. **This will produce two "+ New connection" affordance lines briefly during Commit A's tests until Commit B drops the synthetic row.** Acceptable for the in-between state because nothing real depends on it; the tests for Commit A's renderer changes can either tolerate the duplication OR seed the buffer + drop the synthetic row in their fixture (which still works since visible_rows is unchanged).

Actually, take the simpler path: **for Commit A, gate the new affordance emission behind a `compose_active` (buffer is Some) check.** When the buffer is None, emit nothing extra (the existing synthetic ghost row continues to provide the affordance). When the buffer is Some (which only happens after Commit B), emit the inline prompt. No double affordance.

After Commit B drops the synthetic row, the renderer's affordance emission needs to ALSO produce the static "+ New connection" line when the buffer is None. Commit B's diff updates the gate to "always emit when accounts is expanded."

- [ ] **Step 1: Add the affordance-emission logic**

In `crates/ox-cli/src/settings/renderers/index.rs::render`, after the existing `let items: Vec<ListItem> = rows.iter().enumerate().map(...).collect();` (or wherever the items vector is built), add a post-processing pass:

```rust
// Prepend the inline name prompt when compose mode is active.
// Commit B will extend this to also emit a static "+ New connection"
// affordance when compose is inactive (replacing the synthetic
// AccountAdd ghost row in visible_rows).
let buffer: Option<String> = read_typed(
    ctx.data,
    &oxpath!("ui", "settings", "new_account", "buffer"),
);
if let Some(buf) = buffer {
    // Find the index right after the Accounts header in the items
    // vector. Insert the inline prompt there.
    if let Some(insert_idx) = find_accounts_header_followup_idx(&rows) {
        let prompt = ListItem {
            primary: format!("    Name▸ {}\u{258F}", buf),
            primary_spans: None,
            secondary: None,
            badge: None,
            focus: None,  // decoration; j/k skips it
        };
        // The selected index, if it points at index >= insert_idx
        // (i.e. the focused row is at or after the insertion point),
        // shifts up by 1.
        items.insert(insert_idx, prompt);
        // selected: Option<usize> recompute below — see Step 2.
    }
}
```

Add the helper:

```rust
/// Find the index right AFTER the Accounts entry header in the
/// visible-rows enumeration. Returns `None` if the Accounts entry
/// isn't expanded or doesn't exist. The returned index is the
/// position in the `items` vector where the affordance should be
/// inserted.
fn find_accounts_header_followup_idx(rows: &[VisibleRow]) -> Option<usize> {
    rows.iter()
        .position(|r| {
            matches!(&r.kind, RowKind::Entry { entry_id } if entry_id == "accounts")
                && r.expanded
        })
        .map(|i| i + 1)
}
```

- [ ] **Step 2: Recompute `selected` after insertion**

After the insertion, the `selected: Option<usize>` index needs to be bumped if the focused item moved.

If `selected` was computed before the insertion (the existing code), and the inserted index is `insert_idx`, then:
- `selected = None` → no change.
- `selected = Some(s)` where `s < insert_idx` → no change.
- `selected = Some(s)` where `s >= insert_idx` → `selected = Some(s + 1)`.

Apply this adjustment in the same `if let Some(buf) = buffer { ... }` block, or refactor the earlier `selected` computation to happen after the insertion (cleaner).

Read the existing render function carefully to figure out the cleanest insertion point. The plan can't predict the exact line numbers; the implementer adapts.

- [ ] **Step 3: Run the index renderer tests**

```
cargo test -p ox-cli --lib settings::renderers::index::tests
```

Expected: PASS — the existing tests pass because they don't seed the buffer (so the `if let Some(buf)` branch is dead). If any test explicitly seeds the buffer, it'd hit the new code path; verify the assertion.

### Sub-task 1.5: Commit A

- [ ] **Step 1: Run the full ox-cli lib + e2e + clippy**

```
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
cargo clippy -p ox-cli --all-targets -- -D warnings
```

Expected: all PASS. User-visible behavior is unchanged.

- [ ] **Step 2: Commit**

```
git add -u
git commit -m "feat(settings): add compose-mode infrastructure (dormant)

Adds the four accounts.compose.{insert_char, delete_back, commit,
cancel} commands; bindings at the settings/_compose_new_account
synthetic scope mirroring the existing _edit_mode pattern; the
dispatcher's compose-mode pass that consults
ui/settings/new_account/buffer before falling through to edit-mode
and cursor-scope dispatch; renderer logic that prepends an inline
name prompt when the buffer is Some.

All dormant — nothing writes the buffer yet, so the compose pass
never fires and the renderer's prompt is never emitted. Commit B
flips the switch by rewiring accounts.add and dropping the
synthetic RowKind::AccountAdd ghost row."
```

---

## Task 2: Commit B — switch the substrate, drop the synthetic row + dead code

This is the substantive switch. The compose-mode infrastructure from Commit A activates because `accounts.add` now writes the buffer; the synthetic `RowKind::AccountAdd` row disappears from `visible_rows`; the dead code in `tree::activate`, `edit::commit`, `decorate_row_label`, `edit::insert_char`, and `edit::begin_account_add` gets removed.

### Sub-task 2.1: Rewrite `accounts.add`

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

The current `accounts.add` (after Phase 0): expands the Accounts section if needed, sets `focused = settings/accounts/_new`, and calls `super::edit::begin_account_add()` which writes `edit_field_path = settings/accounts/_new`, `edit_buffer = ""`, `edit_mode = true`.

After this task: writes `Some("")` to `ui/settings/new_account/buffer`, plus ensures the Accounts section is in the expanded set. Does NOT touch `edit_mode`/`edit_buffer`/`edit_field_path`. Does NOT move `focused`.

- [ ] **Step 1: Update the existing `accounts_add_*` tests**

In account_model.rs's tests, the existing `accounts_add_expands_section_focuses_ghost_and_enters_edit` test asserts the old shape. Rewrite it as:

```rust
#[test]
fn accounts_add_writes_buffer_and_expands_section() {
    let mut snap = SettingsSnapshot::empty();
    let writes = run_cmd(&AccountsAdd::new(), &mut snap);
    let by_path: std::collections::BTreeMap<_, _> = writes
        .iter()
        .map(|w| (w.path.to_string(), w.record.clone()))
        .collect();

    // expanded set must contain settings/accounts.
    let exp = by_path.get("ui/settings/expanded").expect("expanded write");
    let set: Vec<String> = match exp {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected: {other:?}"),
    };
    assert!(set.iter().any(|s| s == "settings/accounts"));

    // new_account/buffer must be Some("")
    let buf = by_path
        .get("ui/settings/new_account/buffer")
        .expect("buffer write");
    match buf {
        Record::Parsed(Value::String(s)) => assert!(s.is_empty()),
        other => panic!("expected buffer = Some(\"\"); got {other:?}"),
    }

    // Does NOT write edit_mode, edit_field_path, edit_buffer, or focused.
    assert!(!by_path.contains_key("ui/settings/edit_mode"));
    assert!(!by_path.contains_key("ui/settings/edit_field_path"));
    assert!(!by_path.contains_key("ui/settings/edit_buffer"));
    assert!(!by_path.contains_key("ui/settings/focused"));
}

#[test]
fn accounts_add_preserves_existing_expanded_entries() {
    // Test from Phase 0 stays largely unchanged — the expanded-set
    // preservation invariant holds. Update if its assertions are
    // tighter than this.
}
```

- [ ] **Step 2: Run the tests; expect FAIL**

```
cargo test -p ox-cli --lib settings::commands::account_model::tests::accounts_add_writes_buffer_and_expands_section
```

Expected: FAIL.

- [ ] **Step 3: Rewrite the `AccountsAdd` command's run body**

In `crates/ox-cli/src/settings/commands/account_model.rs`, the current `accounts_add` helper (or the inline `run` body of the `command!` block) reads expanded, ensures `settings/accounts` is in it, writes `focused`, and calls `super::edit::begin_account_add()`. Replace with:

```rust
fn accounts_add(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::visible_rows::{expanded_set_to_value, read_expanded_set};

    let mut expanded = read_expanded_set(data);
    let accounts_key = "settings/accounts".to_string();
    if !expanded.iter().any(|s| s == &accounts_key) {
        expanded.push(accounts_key);
    }

    vec![
        Write {
            path: oxpath!("ui", "settings", "expanded"),
            record: Record::parsed(expanded_set_to_value(&expanded)),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "buffer"),
            record: Record::parsed(Value::String(String::new())),
        },
    ]
}
```

- [ ] **Step 4: Run the tests; expect PASS**

```
cargo test -p ox-cli --lib settings::commands::account_model::tests::accounts_add_writes_buffer_and_expands_section
```

Expected: PASS.

### Sub-task 2.2: Drop the synthetic ghost row from `visible_rows`

**File:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

Drop the `RowKind::AccountAdd` variant + the synthetic-row push at the top of `append_account_rows` + the related tests.

- [ ] **Step 1: Remove the variant**

In `crates/ox-cli/src/settings/visible_rows.rs`'s `RowKind` enum, delete the `AccountAdd` variant. The compiler will surface every match site that handles it.

- [ ] **Step 2: Remove the push in `append_account_rows`**

Delete the `rows.push(VisibleRow { ... kind: RowKind::AccountAdd, ... })` block at the top of `append_account_rows`.

- [ ] **Step 3: Delete the AccountAdd tests in visible_rows**

Delete:
- `expanded_accounts_section_starts_with_account_add_ghost_row`
- `collapsed_accounts_section_has_no_ghost_row`

Update existing tests that asserted row counts including the ghost row:
- `expanded_accounts_inlines_account_rows` — current count is 5 (Accounts header + ghost + 2 accounts + Models header). After drop: 4.
- `position_of_finds_visible_row` — alpha was at index 2 (post-ghost shift). After drop: index 1.

- [ ] **Step 4: Build; surface every AccountAdd match site**

```
cargo build -p ox-cli
```

Expected: FAIL with errors at every site that matches `RowKind::AccountAdd`. Identify them:

- `crates/ox-cli/src/settings/commands/tree.rs::activate` — the `RowKind::AccountAdd => super::edit::begin_account_add()` arm.
- `crates/ox-cli/src/settings/commands/edit.rs::commit` — the `Some(RowKind::AccountAdd) => { ... }` arm.
- `crates/ox-cli/src/settings/commands/edit.rs::insert_char` — the `Some(RowKind::AccountAdd)` accept rule (`Some(RowKind::AccountField { .. }) | Some(RowKind::AccountAdd) => true`).
- `crates/ox-cli/src/settings/renderers/index.rs::decorate_row_label` — the `RowKind::AccountAdd => "Name"` arm.

### Sub-task 2.3: Drop the dead AccountAdd code paths

For each site identified in Sub-task 2.2 Step 4, remove the AccountAdd arm:

- [ ] **Step 1: tree::activate**

In `crates/ox-cli/src/settings/commands/tree.rs::activate`, delete the line:
```rust
RowKind::AccountAdd => super::edit::begin_account_add(),
```

- [ ] **Step 2: edit::commit**

In `crates/ox-cli/src/settings/commands/edit.rs::commit`, delete the entire `Some(RowKind::AccountAdd) => { ... }` arm. The fall-through `_ => Vec::new()` handles cases where `field_path` no longer matches a real row (which shouldn't happen since edit_mode + edit_field_path are only set by begin-edit commands targeting real fields).

Also delete the AccountAdd-specific tests in edit.rs's test module:
- `commit_account_add_writes_account_record_and_cascade`
- `commit_account_add_with_invalid_name_emits_banner_keeps_edit_mode_open`
- `commit_account_add_with_empty_buffer_keeps_edit_mode_open`
- `commit_account_add_with_underscore_prefix_emits_banner_keeps_edit_mode_open`
- `commit_account_add_with_interior_underscore_writes_account_record`

(These tests now live conceptually in account_model.rs's tests for `accounts.compose.commit` — Sub-task 2.4 replaces them there.)

- [ ] **Step 3: edit::insert_char**

In `crates/ox-cli/src/settings/commands/edit.rs::insert_char`, change the accept rule:

Before:
```rust
let accept = match row.as_ref().map(|r| &r.kind) {
    Some(RowKind::ModelField { .. }) => ch.is_ascii_digit(),
    Some(RowKind::AccountField { .. }) | Some(RowKind::AccountAdd) => true,
    _ => false,
};
```

After:
```rust
let accept = match row.as_ref().map(|r| &r.kind) {
    Some(RowKind::ModelField { .. }) => ch.is_ascii_digit(),
    Some(RowKind::AccountField { .. }) => true,
    _ => false,
};
```

- [ ] **Step 4: decorate_row_label**

In `crates/ox-cli/src/settings/renderers/index.rs::decorate_row_label`, delete the `RowKind::AccountAdd => "Name",` arm.

- [ ] **Step 5: Drop `begin_account_add`**

In `crates/ox-cli/src/settings/commands/edit.rs`, delete the `pub(super) fn begin_account_add() -> Vec<Write>` function entirely. No callers remain.

### Sub-task 2.4: Add commit-flow tests for `accounts.compose.commit`

Replace the deleted edit.rs tests with equivalents in account_model.rs's test module, exercising `AccountsComposeCommit::new()` instead of `Commit::new()`.

- [ ] **Step 1: Add tests for the compose-commit flow**

In `crates/ox-cli/src/settings/commands/account_model.rs`'s test module, add (or move from edit.rs):

```rust
#[test]
fn accounts_compose_commit_writes_account_record_and_cascade() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "buffer"),
        Value::String("alpha".into()),
    );
    let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
    let by_path: std::collections::BTreeMap<_, _> = writes
        .iter()
        .map(|w| (w.path.to_string(), w.record.clone()))
        .collect();

    // 1. Account record materialized at config/gate/accounts/alpha.
    let acct = by_path
        .get("config/gate/accounts/alpha")
        .expect("account record write");
    let cfg: ox_gate::AccountConfig = match acct {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected record: {other:?}"),
    };
    assert_eq!(cfg.provider, "anthropic");

    // 2. Buffer cleared (Null write).
    let buf = by_path
        .get("ui/settings/new_account/buffer")
        .expect("buffer cleared");
    assert!(matches!(buf, Record::Parsed(Value::Null)));

    // 3. Selection / cursor / focused / expanded all written.
    assert!(by_path.contains_key("ui/settings/accounts/selected"));
    assert!(by_path.contains_key("ui/settings/cursor"));
    assert!(by_path.contains_key("ui/settings/focused"));
    assert!(by_path.contains_key("ui/settings/expanded"));
}

#[test]
fn accounts_compose_commit_with_empty_buffer_silent_no_op() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "buffer"),
        Value::String("   ".into()),
    );
    let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
    assert!(writes.is_empty());
}

#[test]
fn accounts_compose_commit_with_underscore_prefix_emits_banner() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "buffer"),
        Value::String("_new".into()),
    );
    let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "global", "banner"));
    let banner: ox_types::settings::GlobalBanner = match &writes[0].record {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected: {other:?}"),
    };
    match banner {
        ox_types::settings::GlobalBanner::Error { message, .. } => {
            assert!(message.contains("reserved"));
            assert!(message.contains("_new"));
        }
        other => panic!("expected Error banner; got {other:?}"),
    }
}

#[test]
fn accounts_compose_commit_with_invalid_name_emits_banner() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "buffer"),
        Value::String("bad-name".into()),
    );
    let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "global", "banner"));
}

#[test]
fn accounts_compose_insert_char_appends_to_buffer() {
    use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};

    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "buffer"),
        Value::String("alph".into()),
    );
    // Construct a CommandCtx with last_keystroke = 'a'.
    let registry = crate::settings::registry::RendererRegistry::new();
    let ctx = crate::settings::command_registry::CommandCtx {
        registry: &registry,
        last_keystroke: Some(KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char('a'),
        }),
    };
    let writes = AccountsComposeInsertChar::new().run(&mut snap, &ctx);
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].path,
        oxpath!("ui", "settings", "new_account", "buffer")
    );
    match &writes[0].record {
        Record::Parsed(Value::String(s)) => assert_eq!(s, "alpha"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn accounts_compose_delete_back_pops_buffer() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "buffer"),
        Value::String("alpha".into()),
    );
    let writes = run_cmd(&AccountsComposeDeleteBack::new(), &mut snap);
    assert_eq!(writes.len(), 1);
    match &writes[0].record {
        Record::Parsed(Value::String(s)) => assert_eq!(s, "alph"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn accounts_compose_cancel_clears_buffer() {
    let mut snap = SettingsSnapshot::empty();
    let writes = run_cmd(&AccountsComposeCancel::new(), &mut snap);
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].path,
        oxpath!("ui", "settings", "new_account", "buffer")
    );
    assert!(matches!(&writes[0].record, Record::Parsed(Value::Null)));
}
```

(If `run_cmd` doesn't accept `last_keystroke`, the insert_char test uses the explicit `.run(snap, &ctx)` invocation as shown.)

### Sub-task 2.5: Renderer affordance — always emit when expanded

**File:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs`

Commit A's affordance code only fires when `buffer == Some(_)`. Commit B's drops the synthetic ghost row from `visible_rows`, so the renderer needs to emit the static `+ New connection` line too when buffer is None.

- [ ] **Step 1: Update the affordance code path**

In `index.rs::render`, where Commit A added the `if let Some(buf) = buffer { ... }` block, replace with:

```rust
// Always emit an affordance after the expanded Accounts header.
// Compose mode active → inline name prompt. Inactive → static
// "+ New connection" line.
let buffer: Option<String> = read_typed(
    ctx.data,
    &oxpath!("ui", "settings", "new_account", "buffer"),
);
if let Some(insert_idx) = find_accounts_header_followup_idx(&rows) {
    let primary = match &buffer {
        Some(buf) => format!("    Name▸ {}\u{258F}", buf),
        None => "    + New connection".to_string(),
    };
    let affordance = ListItem {
        primary,
        primary_spans: None,
        secondary: None,
        badge: None,
        focus: None,
    };
    items.insert(insert_idx, affordance);
    // Bump selected if it pointed at index >= insert_idx.
    selected = selected.map(|s| if s >= insert_idx { s + 1 } else { s });
}
```

- [ ] **Step 2: Update tests asserting on rendered items**

Existing renderer tests asserting specific item indexes / counts may shift now that the affordance comes from a different source. Update them where assertions break.

### Sub-task 2.6: Build, test, commit

- [ ] **Step 1: Build**

```
cargo build -p ox-cli
```

Expected: PASS.

- [ ] **Step 2: Run lib + e2e tests + clippy**

```
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
cargo clippy -p ox-cli --all-targets -- -D warnings
```

Expected: PASS. The `add_account_create_flow` e2e test still drives `dispatch("a") → type → dispatch("Enter")`. The new flow handles all three: `a` writes the buffer; characters route through `accounts.compose.insert_char`; Enter routes through `accounts.compose.commit` which writes the AccountConfig. Same end-state.

If any test fails because it asserted on a path or behavior that changed, update the assertion.

- [ ] **Step 3: Commit**

```
git add -u
git commit -m "feat(settings): inline-create lifts into new_account/buffer mode state

accounts.add now writes Some(\"\") to ui/settings/new_account/buffer
(plus ensures the Accounts section is in the expanded set). The
compose-mode dispatcher pass added in commit A activates because
the buffer is now set; printable keys route through
accounts.compose.insert_char; Enter routes through
accounts.compose.commit which validates + writes the AccountConfig
+ UI cascade.

The synthetic RowKind::AccountAdd ghost row is dropped from
visible_rows. Every match site that handled it (tree::activate,
edit::commit, edit::insert_char, decorate_row_label) sheds the
arm; begin_account_add helper is removed.

The renderer now emits the affordance ListItem itself (focus: None,
prepended after the Accounts header) — \"+ New connection\" when
the buffer is None, \"Name▸ <buffer>▏\" when Some. The visible-rows
projection contains only real account rows.

Tests rewritten: AccountAdd-specific tests in edit.rs are deleted
(behavior moved to accounts.compose.* commands); equivalent tests
land in account_model.rs."
```

---

## Task 3: Final verification

- [ ] **Step 1: Full workspace test run**

```
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 2: Clippy**

```
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 3: Verify no stragglers**

```
grep -rn 'RowKind::AccountAdd\|begin_account_add' crates/ 2>/dev/null
```

Expected: zero hits.

```
grep -rn '_new"' crates/ox-cli/src/ 2>/dev/null
```

Expected: any remaining hits must NOT be in cursor scopes or path identifiers; only in test invariants asserting absence (e.g. "no _new path written"), or in glossaries / doc references.

- [ ] **Step 4: Smoke-test in the TUI**

Ask the user to:

1. Open settings.
2. Press `a` — confirm "+ New connection" turns into an inline `Name▸ ▏` prompt.
3. Type a name (e.g. `personal`), press Enter — confirm the new account row appears expanded; focus is on it; no error banner.
4. Press `a` again, press Esc without typing — confirm the prompt collapses back to the static `+ New connection` line; no error.
5. Press `a`, type `_underscore`, press Enter — confirm error banner (transitional rule, retired in Phase 7).
6. Press `a`, type `bad-name`, press Enter — confirm "Invalid account name" banner.

If anything misbehaves, it's a regression — investigate before declaring Phase 3 complete.

---

## Self-review checklist

- [x] `RowKind::AccountAdd` dropped + every match site updated (Sub-tasks 2.2, 2.3).
- [x] `accounts.add` writes `new_account/buffer` instead of focusing the synthetic row (Sub-task 2.1).
- [x] Four new compose-mode commands at `_compose_new_account` synthetic scope (Sub-tasks 1.1, 1.2).
- [x] Dispatcher pass routes to compose scope when buffer is `Some` (Sub-task 1.3).
- [x] Renderer reads buffer + emits affordance with `focus: None` (Sub-task 1.4 + 2.5).
- [x] Tests for the four compose commands pin commit, insert_char, delete_back, cancel (Sub-task 2.4).
- [x] Workspace green + clippy clean + grep clean (Task 3).

Spec requirements not addressed by this plan (intentionally deferred):
- The `_`-prefix banner-error rule survives in `accounts.compose.commit`. After Phase 3 there are no synthetic display paths, so the rule is purely vestigial — Phase 7 retires it.
- The dispatcher's compose-mode pass uses a synthetic cursor scope (`_compose_new_account`) for binding lookup, mirroring the existing `_edit_mode` pattern. The framework's "no synthetic display paths" rule is about visible-row identifiers and user-navigable cursor targets; dispatcher-internal binding-scope keys are an established exception. A future framework cleanup could unify mode dispatch into the inline-handler pattern, but that's out of scope here.
