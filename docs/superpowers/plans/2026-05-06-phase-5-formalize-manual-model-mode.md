# Phase 5: Formalize `manual_model` as a mode + drop `ModelAddManual` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `manual_model/*` sub-tree's existing scattered-atoms shape stays. Three changes: (1) `manual_model/stage` becomes typed (`ManualModelStage` enum) instead of stringly-typed; (2) `manual_model/account` is formally the mode discriminator — the dispatcher's mode-aware pass reads it and routes keys through new compose-mode commands at a `_manual_model` synthetic scope; (3) `RowKind::ModelAddManual` is dropped from `visible_rows`, the renderer reads `manual_model/account` and emits the inline three-stage form decoration when set or a static "+ add model manually (m)" affordance line when not.

**Architecture:** Two-commit landing, mirroring Phases 3 and 4. Commit A introduces the `ManualModelStage` type, the new commands, bindings at `_manual_model`, the dispatcher's manual-model pass — all gated on the stage being typed-enum-shaped (which never happens until Commit B). Commit B switches: rewires the entry point (`m` key → new `models.add_manual` command writing the typed shape), drops `RowKind::ModelAddManual` and its synthetic-row push, drops `tree::activate`'s ModelAddManual arm + `edit::begin_manual_model` + `edit::commit`'s manual_model branch + `edit::cancel`'s manual_model branch, drops the renderer's old ModelAddManual decorate_row_label arm, and adds the new renderer-decoration logic.

**Tech Stack:** Rust workspace; `ox-types` (new enum), `ox-cli` (commands, bindings, dispatcher, renderer, visible_rows, tests).

**Spec:** [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md) §5 Phase 5.

---

## File Map

**Modify:**
- `crates/ox-types/src/settings.rs` — add `ManualModelStage` enum.
- `crates/ox-cli/src/settings/commands/account_model.rs` — add `models.add_manual` + `models.compose_manual.{insert_char, delete_back, commit, cancel}` commands + helpers; register them.
- `crates/ox-cli/src/settings/bindings.rs` — add `register_manual_model` (synthetic-scope bindings for printable/Backspace/Enter/Esc) + `m` binding at `Prefix(settings/models)`.
- `crates/ox-cli/src/settings/dispatch.rs` — add the manual-model pass (between pending-delete and compose).
- `crates/ox-cli/src/settings/renderers/index.rs` — read `manual_model/account` for inline form decoration; drop `RowKind::ModelAddManual` arm in `decorate_row_label` (Commit B).
- `crates/ox-cli/src/settings/visible_rows.rs` — drop `RowKind::ModelAddManual` variant + the synthetic-row push in `append_model_rows` + the related test (Commit B).
- `crates/ox-cli/src/settings/commands/tree.rs` — drop `RowKind::ModelAddManual` arm in `activate` (Commit B).
- `crates/ox-cli/src/settings/commands/edit.rs` — drop `begin_manual_model` helper, `commit_manual_model` helper, the manual_model branches in `commit` and `cancel`, the manual_model accept-rule subtleties in `insert_char` (Commit B).

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section comments in code.

---

## Task 1: Commit A — add manual-model infrastructure (dormant)

This task adds all the new pieces. The dispatcher pass is gated on the stage being typed-enum-shaped (i.e., `manual_model/stage` deserializes as `ManualModelStage`, not as the existing `String`). Since nothing writes the typed shape yet, the new pass is dormant.

### Sub-task 1.1: Add `ManualModelStage` to ox-types

**File:**
- Modify: `crates/ox-types/src/settings.rs`

- [ ] **Step 1: Add the type**

Near the existing `AccountField` / `ModelField` / `ModelKey` definitions, add:

```rust
/// The current stage of the manual-model entry form.
///
/// The form is a three-step state machine: the user types a model id,
/// then a context-window size, then a max-output-tokens size. Each
/// stage's commit advances to the next; the final stage's commit
/// finalizes the new `ModelInfo` into the account's catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualModelStage {
    Id,
    Ctx,
    Out,
}
```

The `#[serde(rename_all = "snake_case")]` makes it serialize as `"id"` / `"ctx"` / `"out"` — same wire shape as the existing stringly-typed value. (This is intentional — the typed-vs-stringly distinction in this plan is about WHO produces and consumes the value, not the wire format. Old write sites that produce a String will fail to deserialize as ManualModelStage; new write sites that produce ManualModelStage will deserialize cleanly. So coexistence works during the Commit A → Commit B transition.)

Wait — that's wrong. If both produce the same wire format `"id"`, they're indistinguishable. The dispatcher's gating ("only fire pass when stage is typed-enum-shaped") doesn't work because both shapes look identical on the wire.

Use a different rename to disambiguate:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ManualModelStage {
    Id,
    Ctx,
    Out,
}
```

Now serializes as `"Id"` / `"Ctx"` / `"Out"` — distinct from the existing `"id"` / `"ctx"` / `"out"` strings. The dispatcher attempts to deserialize `manual_model/stage` as `ManualModelStage`; succeeds only when the value is the new shape; fails (and falls through) when the value is the legacy string.

- [ ] **Step 2: Build**

```
cargo build -p ox-types
cargo build --workspace
```

Expected: PASS.

### Sub-task 1.2: Add the four `models.compose_manual.*` commands + entry command

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Add the entry command**

```rust
command! {
    struct_name: ModelsAddManual,
    id: "models.add_manual",
    title: "Add Model Manually",
    description: "Open the inline three-stage manual-model entry form for the focused account.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_add_manual(snap),
}
```

- [ ] **Step 2: Add the four mode commands**

```rust
command! {
    struct_name: ModelsManualInsertChar,
    id: "models.compose_manual.insert_char",
    title: "Insert character (manual model)",
    description: "Append the just-pressed char to the manual-model buffer (per-stage rules).",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, ctx| models_manual_insert_char(snap, ctx),
}

command! {
    struct_name: ModelsManualDeleteBack,
    id: "models.compose_manual.delete_back",
    title: "Backspace (manual model)",
    description: "Pop the last character from the manual-model buffer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| models_manual_delete_back(snap),
}

command! {
    struct_name: ModelsManualCommit,
    id: "models.compose_manual.commit",
    title: "Commit stage (manual model)",
    description: "Advance the form's stage; the final stage finalizes the new ModelInfo into the catalog.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| models_manual_commit(snap),
}

command! {
    struct_name: ModelsManualCancel,
    id: "models.compose_manual.cancel",
    title: "Cancel manual model",
    description: "Discard the manual-model buffer and exit compose mode.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| models_manual_cancel(snap),
}
```

- [ ] **Step 3: Add the helpers**

```rust
fn models_add_manual(data: &mut dyn Reader) -> Vec<Write> {
    use ox_types::settings::ManualModelStage;
    use crate::settings::visible_rows::{enumerate, RowKind};

    // Read the focused row to figure out which account to compose for.
    let focused = match crate::settings::commands::navigation::path_from_value(
        data.read(&oxpath!("ui", "settings", "focused"))
            .ok()
            .flatten()
            .as_ref()
            .and_then(|r| r.as_value())
            .unwrap_or(&Value::Null),
    ) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let rows = enumerate(data);
    let account = rows
        .iter()
        .find(|r| r.path == focused)
        .and_then(|r| match &r.kind {
            RowKind::Model { account, .. } => Some(account.clone()),
            RowKind::ModelEmptyState { account } => Some(account.clone()),
            RowKind::ModelField { account, .. } => Some(account.clone()),
            _ => None,
        });
    let Some(account) = account else {
        return Vec::new();
    };

    let stage_value = match to_value(&ManualModelStage::Id) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    vec![
        Write {
            path: oxpath!("ui", "settings", "manual_model", "account"),
            record: Record::parsed(Value::String(account)),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "stage"),
            record: Record::parsed(stage_value),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "buffer"),
            record: Record::parsed(Value::String(String::new())),
        },
    ]
}

fn models_manual_read_stage(data: &mut dyn Reader) -> Option<ox_types::settings::ManualModelStage> {
    read_typed(data, &oxpath!("ui", "settings", "manual_model", "stage"))
}

fn models_manual_insert_char(
    data: &mut dyn Reader,
    ctx: &super::command_registry::CommandCtx<'_>,
) -> Vec<Write> {
    use ox_types::key_chord::KeyCodeRepr;
    use ox_types::settings::ManualModelStage;

    let chord = match ctx.last_keystroke.as_ref() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let ch = match chord.code {
        KeyCodeRepr::Char(c) => c,
        _ => return Vec::new(),
    };
    let stage = match models_manual_read_stage(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    // Per-stage accept rules: Id accepts any printable char; Ctx and
    // Out accept ASCII digits only (the values are u32 sizes).
    let accept = match stage {
        ManualModelStage::Id => true,
        ManualModelStage::Ctx | ManualModelStage::Out => ch.is_ascii_digit(),
    };
    if !accept {
        return Vec::new();
    }
    let current: String = read_typed(
        data,
        &oxpath!("ui", "settings", "manual_model", "buffer"),
    )
    .unwrap_or_default();
    let mut next = current;
    next.push(ch);
    vec![Write {
        path: oxpath!("ui", "settings", "manual_model", "buffer"),
        record: Record::parsed(Value::String(next)),
    }]
}

fn models_manual_delete_back(data: &mut dyn Reader) -> Vec<Write> {
    let mut current: String = read_typed(
        data,
        &oxpath!("ui", "settings", "manual_model", "buffer"),
    )
    .unwrap_or_default();
    if current.pop().is_none() {
        return Vec::new();
    }
    vec![Write {
        path: oxpath!("ui", "settings", "manual_model", "buffer"),
        record: Record::parsed(Value::String(current)),
    }]
}

fn models_manual_commit(data: &mut dyn Reader) -> Vec<Write> {
    use ox_gate::{ModelInfo, ModelInfoSource};
    use ox_kernel::PathComponent;
    use ox_types::settings::ManualModelStage;

    let stage = match models_manual_read_stage(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let buffer: String = read_typed(
        data,
        &oxpath!("ui", "settings", "manual_model", "buffer"),
    )
    .unwrap_or_default();
    let trimmed = buffer.trim();

    match stage {
        ManualModelStage::Id => {
            if trimmed.is_empty() {
                return Vec::new();
            }
            let next_stage = match to_value(&ManualModelStage::Ctx) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            vec![
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(next_stage),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "staged_id"),
                    record: Record::parsed(Value::String(trimmed.to_string())),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "buffer"),
                    record: Record::parsed(Value::String(String::new())),
                },
            ]
        }
        ManualModelStage::Ctx => {
            let n: u32 = match trimmed.parse() {
                Ok(n) if n > 0 => n,
                _ => return Vec::new(),
            };
            let next_stage = match to_value(&ManualModelStage::Out) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            vec![
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(next_stage),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "staged_ctx"),
                    record: Record::parsed(Value::String(n.to_string())),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "buffer"),
                    record: Record::parsed(Value::String(String::new())),
                },
            ]
        }
        ManualModelStage::Out => {
            let out: u32 = match trimmed.parse() {
                Ok(n) if n > 0 => n,
                _ => return Vec::new(),
            };
            let id: String = read_typed(
                data,
                &oxpath!("ui", "settings", "manual_model", "staged_id"),
            )
            .unwrap_or_default();
            let ctx: u32 = read_typed::<String>(
                data,
                &oxpath!("ui", "settings", "manual_model", "staged_ctx"),
            )
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
            let account: String = read_typed(
                data,
                &oxpath!("ui", "settings", "manual_model", "account"),
            )
            .unwrap_or_default();
            let comp = match PathComponent::try_new(&account) {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            };

            let catalog_path = oxpath!("config", "gate", "accounts", comp, "models");
            let mut catalog: Vec<ModelInfo> =
                read_typed(data, &catalog_path).unwrap_or_default();
            catalog.push(ModelInfo {
                id: id.clone(),
                display_name: id,
                max_context_size: Some(ctx),
                max_output_tokens: Some(out),
                source: ModelInfoSource::UserEntered,
            });
            let catalog_value = match to_value(&catalog) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };

            // Write the catalog and clear the form state.
            vec![
                Write {
                    path: catalog_path,
                    record: Record::parsed(catalog_value),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "account"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "buffer"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "staged_id"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "staged_ctx"),
                    record: Record::parsed(Value::Null),
                },
            ]
        }
    }
}

fn models_manual_cancel(_data: &mut dyn Reader) -> Vec<Write> {
    // Clear all manual_model paths.
    vec![
        Write {
            path: oxpath!("ui", "settings", "manual_model", "account"),
            record: Record::parsed(Value::Null),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "stage"),
            record: Record::parsed(Value::Null),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "buffer"),
            record: Record::parsed(Value::Null),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "staged_id"),
            record: Record::parsed(Value::Null),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "staged_ctx"),
            record: Record::parsed(Value::Null),
        },
    ]
}
```

- [ ] **Step 4: Register the commands**

In account_model.rs's `register` function, add registrations for all five new commands.

- [ ] **Step 5: Build**

```
cargo build -p ox-cli
```

Expected: PASS.

### Sub-task 1.3: Add bindings

**File:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Add `register_manual_model`**

```rust
fn register_manual_model(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_manual_model");
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
            command_id: cmd("models.compose_manual.insert_char"),
        });
    }
    bind(reg, Some(scope.clone()), no_mods(), KeyCodeRepr::Backspace, "models.compose_manual.delete_back");
    bind(reg, Some(scope.clone()), no_mods(), KeyCodeRepr::Enter, "models.compose_manual.commit");
    bind(reg, Some(scope), no_mods(), KeyCodeRepr::Esc, "models.compose_manual.cancel");
}
```

- [ ] **Step 2: Add `m` binding at Prefix(settings/models)**

In the existing `register_row_prefixes` function (or wherever `Prefix(settings/models)` bindings live — search for `bind_prefix(reg, models_subtree, ...)` calls), add:

```rust
bind_prefix(
    reg,
    models_subtree.clone(),
    no_mods(),
    KeyCodeRepr::Char('m'),
    "models.add_manual",
);
```

- [ ] **Step 3: Call `register_manual_model` from the top-level register**

Add `register_manual_model(reg);` alongside the other synthetic-scope register calls.

- [ ] **Step 4: Build**

```
cargo build -p ox-cli
```

Expected: PASS.

### Sub-task 1.4: Dispatcher's manual-model pass

**File:**
- Modify: `crates/ox-cli/src/settings/dispatch.rs`

- [ ] **Step 1: Add the discriminator helper**

```rust
/// Read `ui/settings/manual_model/stage` as the typed `ManualModelStage`
/// enum. Returns `Some` only when the stored value is the typed shape
/// (the wire format uses PascalCase: "Id" / "Ctx" / "Out") — falls
/// through if the legacy stringly-typed value ("id" / "ctx" / "out")
/// is present, so old flow continues to route through edit-mode pass
/// during the transition.
fn read_manual_model_active(snapshot: &mut dyn Reader) -> bool {
    use ox_path::oxpath;
    use ox_types::settings::ManualModelStage;
    let record = match snapshot
        .read(&oxpath!("ui", "settings", "manual_model", "stage"))
        .ok()
        .flatten()
    {
        Some(r) => r,
        None => return false,
    };
    let value = match record.as_value() {
        Some(v) => v.clone(),
        None => return false,
    };
    structfs_serde_store::from_value::<ManualModelStage>(value).is_ok()
}
```

- [ ] **Step 2: Extend the dispatch chain**

In `dispatch_settings_key`, add a manual-model pass between pending-delete and compose:

```rust
let pending_delete_active = read_pending_delete(snapshot).is_some();
let pending_delete_scope = ox_path::oxpath!("settings", "_pending_delete");
let manual_model_active = read_manual_model_active(snapshot);
let manual_model_scope = ox_path::oxpath!("settings", "_manual_model");
let compose_active = read_compose_buffer(snapshot).is_some();
// ... rest unchanged ...

let cmd_id = if pending_delete_active {
    bindings.lookup(screen, &pending_delete_scope, mode, key)
} else {
    None
}
.or_else(|| {
    if manual_model_active {
        bindings.lookup(screen, &manual_model_scope, mode, key)
    } else {
        None
    }
})
.or_else(|| { /* compose */ })
.or_else(|| { /* edit */ })
.or_else(|| { /* focused-row */ })
.or_else(|| bindings.lookup(screen, cursor, mode, key));
```

Update the comment block to list six passes.

- [ ] **Step 3: Add a unit test**

Mirror the `pending_delete_routes_to_*` test pattern: seed `manual_model/stage` with a serialized `ManualModelStage::Id` value, register a binding at `_manual_model` scope, dispatch a key, assert the binding fires.

```rust
#[test]
fn manual_model_routes_to_manual_model_scope_when_typed_stage_set() {
    use ox_types::settings::ManualModelStage;

    let mut cmds = CommandRegistry::new();
    cmds.register(Box::new(WriteSentinel::new()));

    let mut bindings = BindingRegistry::new();
    bindings.register(BindingEntry {
        screen: Screen::Settings,
        scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
        mode: None,
        key: key_char('a'),
        command_id: cmd_id("test.sentinel"),
    });

    let renderers = RendererRegistry::new();
    let mut reader = LocalConfig::default();
    reader
        .write(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Record::parsed(structfs_serde_store::to_value(&ManualModelStage::Id).unwrap()),
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
fn manual_model_falls_through_when_legacy_stringly_stage() {
    // The old flow (begin_manual_model) writes Value::String("id") at
    // the stage path. The new dispatcher pass deserializes the value as
    // ManualModelStage; when it fails, falls through to edit-mode /
    // page-cursor passes — preserving the old flow's behavior during
    // the Commit A → Commit B transition.
    let cmds = CommandRegistry::new();
    let mut bindings = BindingRegistry::new();
    bindings.register(BindingEntry {
        screen: Screen::Settings,
        scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
        mode: None,
        key: key_char('a'),
        command_id: cmd_id("test.sentinel"),
    });

    let renderers = RendererRegistry::new();
    let mut reader = LocalConfig::default();
    reader
        .write(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Record::parsed(Value::String("id".into())),
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

    // The new pass shouldn't fire — the value isn't the typed shape.
    // Bindings would fall through; with no other binding registered,
    // result is empty.
    assert!(writes.is_empty());
}
```

- [ ] **Step 4: Run dispatch tests**

```
cargo test -p ox-cli --lib settings::dispatch::tests
```

Expected: PASS.

### Sub-task 1.5: Commit A

- [ ] **Step 1: Run lib + e2e + clippy**

```
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
cargo clippy -p ox-cli --all-targets -- -D warnings
```

Expected: all PASS. Old manual-model flow continues to work (stage stays stringly-typed; new pass falls through; old edit-mode pass intercepts as before).

- [ ] **Step 2: Commit**

```
git add -u
git commit -m "feat(settings): add manual-model infrastructure (dormant)

Adds ManualModelStage enum (PascalCase wire format, distinct from
the legacy stringly-typed values); five new commands —
models.add_manual (entry, bound to 'm' at Prefix(settings/models))
plus models.compose_manual.{insert_char, delete_back, commit,
cancel} at the synthetic _manual_model scope; the dispatcher's
manual-model pass that consults manual_model/stage and routes to
_manual_model when the typed shape is present.

All dormant — the old flow writes the legacy stringly-typed stage
value, which doesn't deserialize as ManualModelStage, so the new
pass falls through. Commit B switches the entry point and retires
the old flow."
```

---

## Task 2: Commit B — switch substrate, drop synthetic row + dead code

### Sub-task 2.1: Drop `RowKind::ModelAddManual`

**File:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Remove the variant**

Delete the `ModelAddManual { account: String }` variant from `RowKind`.

- [ ] **Step 2: Remove the synthetic-row push**

In `append_model_rows`, the existing code pushes a `ModelAddManual` row immediately after a `ModelEmptyState` row for empty-catalog accounts. Delete that push.

- [ ] **Step 3: Remove the test**

Delete `empty_state_is_followed_by_add_manual_row` (around visible_rows.rs:1239+).

Update any test that asserted "row count includes both empty-state and add-manual" — those are now empty-state-only.

### Sub-task 2.2: Drop `tree::activate`'s ModelAddManual arm

**File:**
- Modify: `crates/ox-cli/src/settings/commands/tree.rs`

- [ ] **Step 1: Remove the arm**

Delete the line:
```rust
RowKind::ModelAddManual { account } => super::edit::begin_manual_model(data, account),
```

The compiler will surface other ModelAddManual references to clean up.

### Sub-task 2.3: Drop `edit::begin_manual_model` and the manual_model branches

**File:**
- Modify: `crates/ox-cli/src/settings/commands/edit.rs`

- [ ] **Step 1: Drop `begin_manual_model`**

Delete the `pub(crate) fn begin_manual_model` helper (around line 231). No callers remain after sub-task 2.2.

- [ ] **Step 2: Drop the manual_model branch in `commit`**

In `edit.rs::commit`, the current code starts with a manual_model preempt:

```rust
if let Some(stage) = ... read manual_model/stage ... {
    return commit_manual_model(data, &stage);
}
```

Delete that block entirely.

- [ ] **Step 3: Drop the manual_model branch in `cancel`**

In `edit.rs::cancel`, the current code first nulls all manual_model paths if a stage is set. Delete that pre-block:

```rust
if read_typed::<String>(data, &oxpath!("ui", "settings", "manual_model", "stage")).is_some() {
    for sub in ["account", "stage", "buffer", "staged_id", "staged_ctx"] {
        ...
    }
}
```

Delete the whole if-block. The remainder (`writes.extend(clear_edit_state())`) stays.

- [ ] **Step 4: Drop `commit_manual_model`**

Delete the entire `commit_manual_model` helper function (around line 421+).

- [ ] **Step 5: Drop manual_model tests**

Delete the manual_model-specific tests in edit.rs's test module:
- `manual_model_commit_id_advances_to_ctx_stage`
- `manual_model_commit_id_rejects_empty`
- `manual_model_commit_ctx_rejects_non_numeric`
- `manual_model_cancel_clears_form_without_writing_catalog`
- `manual_model_commit_out_writes_full_modelinfo_and_clears_form`

(Equivalent tests will move to account_model.rs's tests for the new commands — Sub-task 2.5.)

### Sub-task 2.4: Update the renderer

**File:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs`

- [ ] **Step 1: Drop the ModelAddManual decorate_row_label arm**

In `decorate_row_label`, delete any `RowKind::ModelAddManual` arm. (May not exist depending on the existing implementation — the manual-model decoration was rendered by reading manual_model/* paths, but the row's plain label would have read "+ add model manually". Verify by reading the function and only removing matchen on ModelAddManual.)

- [ ] **Step 2: Add the new affordance/form decoration**

After the existing `pending_delete` banner-prepend logic, add manual-model decoration. The renderer needs to:
- For each `RowKind::ModelEmptyState` row in the items vector, insert a decoration ListItem immediately after it.
- The decoration content depends on `manual_model/account`:
  - If equals this account's name: render the inline form prompt (per stage).
  - Otherwise: render the static "+ add model manually (m)" line.

```rust
let manual_account: Option<String> = read_typed(
    ctx.data,
    &oxpath!("ui", "settings", "manual_model", "account"),
);

// Find every ModelEmptyState row in the rows vector and remember its
// position in the items vector. Then iterate in REVERSE order to
// insert affordance/form ListItems without invalidating earlier
// indices.
let mut empty_state_positions: Vec<(usize, String)> = Vec::new();
for (i, row) in rows.iter().enumerate() {
    if let RowKind::ModelEmptyState { account } = &row.kind {
        empty_state_positions.push((i, account.clone()));
    }
}
for (i, account) in empty_state_positions.iter().rev() {
    let in_mode_for_this_account = manual_account.as_deref() == Some(account.as_str());
    let primary = if in_mode_for_this_account {
        let stage = read_typed::<ox_types::settings::ManualModelStage>(
            ctx.data,
            &oxpath!("ui", "settings", "manual_model", "stage"),
        );
        let buffer: String = read_typed(
            ctx.data,
            &oxpath!("ui", "settings", "manual_model", "buffer"),
        )
        .unwrap_or_default();
        let prompt = match stage {
            Some(ox_types::settings::ManualModelStage::Id) => "Model id",
            Some(ox_types::settings::ManualModelStage::Ctx) => "Max context",
            Some(ox_types::settings::ManualModelStage::Out) => "Max output",
            None => "Model id",
        };
        format!("    {prompt}▸ {buffer}\u{258F}")
    } else {
        format!("    + add model manually (m)")
    };
    let decoration = ListItem {
        primary,
        primary_spans: None,
        secondary: None,
        badge: None,
        focus: None,
    };
    items.insert(i + 1, decoration);
    // Bump selected if it pointed at index >= i + 1.
    selected = selected.map(|s| if s >= i + 1 { s + 1 } else { s });
}
```

- [ ] **Step 3: Run renderer tests**

```
cargo test -p ox-cli --lib settings::renderers::index::tests
```

Expected: PASS or surface tests with shifted indices that need updates.

### Sub-task 2.5: Add tests for the new commands

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`'s tests.

Replace the deleted edit.rs tests with equivalents driving the new `Models*Manual*` commands. Cover:
- Stage-Id commit advances stage to Ctx, writes staged_id.
- Stage-Id commit with empty buffer is no-op.
- Stage-Ctx commit advances stage to Out with parsed u32.
- Stage-Ctx commit with non-numeric is no-op.
- Stage-Out commit writes the catalog + clears all manual_model state.
- Cancel clears all manual_model state.
- Insert_char in stage Ctx accepts digits, rejects letters.
- Insert_char in stage Id accepts any printable.
- Add_manual writes account/stage/buffer based on focused row.

Adapt the deleted tests' bodies to the new command names + typed stage values (use `to_value(&ManualModelStage::Ctx)` etc. for setup).

### Sub-task 2.6: Build, test, commit

- [ ] **Step 1: Build + test + clippy**

```
cargo build -p ox-cli
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 2: Commit**

```
git add -u
git commit -m "feat(settings): manual-model lifts into typed-stage mode-state

The five-path manual_model/* sub-tree's scattered-atoms shape
stays. manual_model/stage promotes from stringly-typed
\"id\"/\"ctx\"/\"out\" to ManualModelStage enum (PascalCase wire
format). manual_model/account is formally the mode discriminator —
the dispatcher's manual-model pass routes y/n/Esc/printable through
the new models.compose_manual.* commands when the typed stage is
present.

RowKind::ModelAddManual is dropped from visible_rows; the
synthetic-row push retires; tree::activate sheds its arm;
edit::begin_manual_model + edit::commit_manual_model + the
manual_model branches in edit::commit and edit::cancel are gone.

The renderer reads manual_model/account and emits a per-stage
inline form prompt when active for an account, or a static
\"+ add model manually (m)\" affordance below each empty-catalog
account when inactive. Phase 6 lifts the empty-state row into
similar renderer-side decoration."
```

---

## Task 3: Final verification

- [ ] **Step 1: Workspace tests**

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
grep -rn 'RowKind::ModelAddManual\|begin_manual_model\|commit_manual_model' crates/ 2>/dev/null
```

Expected: zero hits.

- [ ] **Step 4: Smoke-test in the TUI**

Ask the user to:

1. Open settings → expand the Models entry.
2. Find a connection with no models. Verify the "(no models — Enter to refresh)" line and the "+ add model manually (m)" line both appear below the connection row.
3. Press `m` while focused on a row in that connection's empty section.
4. Verify the affordance line transforms into `Model id▸ ▏` (stage 1 prompt).
5. Type a model id (e.g. `claude-haiku-4`), press Enter.
6. Verify it transforms to `Max context▸ ▏` (stage 2).
7. Type `200000`, press Enter.
8. Verify it transforms to `Max output▸ ▏` (stage 3).
9. Type `8000`, press Enter.
10. Verify the model row appears in the connection's catalog; the form is gone; the empty-state line is gone (the connection now has a model).

If anything misbehaves, regression — investigate.

---

## Self-review checklist

- [x] `ManualModelStage` enum added (PascalCase wire format) (Sub-task 1.1).
- [x] Five new commands at `_manual_model` synthetic scope + `m` at Prefix(settings/models) (Sub-tasks 1.2, 1.3).
- [x] Dispatcher routes per typed stage (Sub-task 1.4).
- [x] Renderer reads manual_model/account; emits inline form when active, affordance line when not (Sub-task 2.4).
- [x] `RowKind::ModelAddManual` dropped (Sub-task 2.1).
- [x] `tree::activate`'s ModelAddManual arm dropped (Sub-task 2.2).
- [x] `edit::begin_manual_model`, `commit_manual_model`, manual_model branches in commit/cancel all dropped (Sub-task 2.3).
- [x] Workspace green + clippy clean + grep clean (Task 3).
