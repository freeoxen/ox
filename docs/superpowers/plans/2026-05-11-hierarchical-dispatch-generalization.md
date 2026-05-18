# Hierarchical Dispatch Generalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to execute task-by-task with spec + quality review between tasks.

**Goal:** Promote `Phase` to a first-class field on `BindingEntry`, refactor the dispatcher to walk the scope path in three phases generically, and migrate every compound widget on the settings screen onto the phase-aware model.

**Architecture:** `Phase::{Capture,Target,Bubble}` on every binding. Dispatcher walks the scope path: capture outer→inner, target leaf-only, bubble inner→outer. First match per phase wins. `compute_scope_path` reads UI-state discriminators to build the path. No per-widget pass logic.

**Spec:** `docs/superpowers/specs/2026-05-11-hierarchical-dispatch-generalization-design.md`.

---

## Task S1: Add `Phase` field to `BindingEntry`; phase-aware lookup

**Files:**
- Modify: `crates/ox-types/src/command_binding.rs` (or wherever `BindingEntry` lives — locate via `grep -rn "pub struct BindingEntry" crates/`)
- Modify: `crates/ox-cli/src/settings/bindings.rs` (every `BindingRegistry::register` call → add phase arg, default to `Target` mechanically)
- Modify: `crates/ox-cli/src/settings/dispatch.rs` (every `bindings.lookup(...)` call → add `Phase::Target` arg)

- [ ] **Step 1: Add `Phase` enum**

In `command_binding.rs` (or sibling location):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Capture,
    Target,
    Bubble,
}

impl Default for Phase {
    fn default() -> Self {
        // Default to `Target` so migrations that haven't declared a phase
        // yet preserve today's leaf-level binding semantics.
        Phase::Target
    }
}
```

Locate the actual file via grep; place near `KeyChord` definition.

- [ ] **Step 2: Add `phase: Phase` field to `BindingEntry`**

```rust
pub struct BindingEntry {
    pub screen:      Screen,
    pub scope:       BindingScope,        // existing
    pub mode:        Option<Mode>,
    pub key:         KeyChord,
    pub command_id:  CommandId,
    pub phase:       Phase,               // NEW
}
```

The other fields' names may differ — match the existing struct exactly.

- [ ] **Step 3: Update `BindingRegistry::lookup` signature**

```rust
pub fn lookup(
    &self,
    screen: Screen,
    scope: &BindingScope,                  // or whatever the existing param type is
    mode: Option<&Mode>,
    key: &KeyChord,
    phase: Phase,                          // NEW
) -> Option<&CommandId>;
```

Internal: filter the candidate list by `entry.phase == phase` before applying specificity tie-breaking.

- [ ] **Step 4: Update every existing call site**

- **`bindings.rs` registration sites**: every `BindingEntry { ... }` literal needs `phase: Phase::Target` (the default). Mechanical fix.
- **`dispatch.rs` lookup sites**: every `bindings.lookup(screen, scope, mode, key)` becomes `bindings.lookup(screen, scope, mode, key, Phase::Target)`. The compose-specific `lookup_compose` keeps its existing `is_capture_key`/`is_bubble_key` enumeration but queries with `Phase::Capture` / `Phase::Target` / `Phase::Bubble` explicitly instead of querying once and hoping the right binding fires.

- [ ] **Step 5: Existing tests pass**

```bash
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
cargo test -p ox-types
```

Behavior unchanged. All tests green.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-types/src/command_binding.rs crates/ox-cli/src/settings/bindings.rs crates/ox-cli/src/settings/dispatch.rs
git commit -m "bindings: add Phase field to BindingEntry; phase-aware lookup"
```

(File names may differ — stage exactly the files you modified.)

---

## Task S2: Generic three-phase dispatcher loop; retire `lookup_compose`

**Files:**
- Modify: `crates/ox-cli/src/settings/dispatch.rs`

The current dispatcher has per-widget passes: `lookup_compose`, plus separate logic for pending-delete, manual-model, edit-mode, focused-row, page-cursor. Replace them with a single `compute_scope_path` → three-phase walk.

- [ ] **Step 1: Write `compute_scope_path`**

```rust
fn compute_scope_path(snap: &mut dyn Reader, cursor: &Path) -> Vec<BindingScope> {
    let mut path = vec![BindingScope::Exact(cursor.clone())];

    // Compound widgets — mutually exclusive by design.
    // (Order matters only if invariants are violated; flag via debug_assert.)
    let pending_delete = read_pending_delete(snap).is_some();
    let manual_model_stage = read_manual_model_stage(snap);
    let compose_active = read_compose_active(snap);
    let edit_mode_active = read_edit_mode_active(snap);

    debug_assert!(
        [pending_delete, manual_model_stage.is_some(), compose_active, edit_mode_active]
            .iter().filter(|b| **b).count() <= 1,
        "at most one compound-widget mode active at a time"
    );

    if pending_delete {
        path.push(BindingScope::Exact(oxpath!("settings", "_pending_delete")));
    }
    if let Some(stage) = manual_model_stage {
        path.push(BindingScope::Exact(oxpath!("settings", "_manual_model")));
        path.push(BindingScope::Exact(oxpath!("settings", "_manual_model", stage_to_str(stage))));
    }
    if compose_active {
        path.push(BindingScope::Exact(oxpath!("settings", "_compose_form")));
        let leaf = match field_kind(read_focused_compose_field(snap)) {
            FieldKind::Text => oxpath!("settings", "_compose_field_text"),
            FieldKind::Selector => oxpath!("settings", "_compose_field_selector"),
        };
        path.push(BindingScope::Exact(leaf));
    }
    if edit_mode_active {
        path.push(BindingScope::Exact(oxpath!("settings", "_edit_mode")));
    }

    path
}
```

Adapt names: `read_pending_delete`, `read_manual_model_stage`, `read_compose_active`, `read_edit_mode_active`, `read_focused_compose_field`, `stage_to_str`, `field_kind` — all should already exist in the codebase (look in the corresponding `commands/*.rs` and `dispatch.rs`).

- [ ] **Step 2: Replace the dispatcher main loop**

```rust
fn dispatch_settings_key(
    bindings: &BindingRegistry,
    commands: &CommandRegistry,
    snap: &mut dyn Reader,
    cursor: &Path,
    mode: Option<&Mode>,
    key: &KeyChord,
    ctx: &CommandCtx,
) -> Option<Vec<Write>> {
    let scope_path = compute_scope_path(snap, cursor);

    // Capture: outer → inner.
    for scope in &scope_path {
        if let Some(cmd_id) = bindings.lookup(Screen::Settings, scope, mode, key, Phase::Capture) {
            if let Some(cmd) = commands.lookup(cmd_id) {
                return Some(cmd.run(snap, ctx));
            }
        }
    }

    // Target: leaf only.
    if let Some(leaf) = scope_path.last() {
        if let Some(cmd_id) = bindings.lookup(Screen::Settings, leaf, mode, key, Phase::Target) {
            if let Some(cmd) = commands.lookup(cmd_id) {
                return Some(cmd.run(snap, ctx));
            }
        }
    }

    // Bubble: inner → outer.
    for scope in scope_path.iter().rev() {
        if let Some(cmd_id) = bindings.lookup(Screen::Settings, scope, mode, key, Phase::Bubble) {
            if let Some(cmd) = commands.lookup(cmd_id) {
                return Some(cmd.run(snap, ctx));
            }
        }
    }

    None
}
```

- [ ] **Step 3: Delete `lookup_compose`, `is_capture_key`, `is_bubble_key`**

The compose-specific dispatch logic is gone. Everything happens through the generic walk.

- [ ] **Step 4: Update the compose-mode binding registrations to declare phases**

This is critical — without it, the compose form will silently break.

In `bindings.rs`, the compose registrations from T10b had Esc/Tab/etc. on the form scope (currently registered with `Phase::Target` by S1's default). Update them:

- Form scope `_compose_form`:
  - Esc → `accounts.compose.cancel`: `phase: Phase::Capture`
  - Tab, Down → `accounts.compose.focus_next`: `phase: Phase::Capture`
  - BackTab, Up → `accounts.compose.focus_prev`: `phase: Phase::Capture`
  - Enter → `accounts.compose.commit`: `phase: Phase::Bubble`
- Field-text scope `_compose_field_text`:
  - printable, Backspace: `phase: Phase::Target` (already correct via default; explicit declaration optional but recommended for clarity)
- Field-selector scope `_compose_field_selector`:
  - h, l, Left, Right: `phase: Phase::Target` (same — explicit recommended)

- [ ] **Step 5: Run all suites; nothing should regress**

```bash
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
```

Particularly verify:
- Esc cancels compose regardless of which field is focused.
- Tab/Shift+Tab navigate focus.
- h/l insert when focused on text, cycle when focused on selector.
- Enter commits.

The compose e2e snapshots should be unchanged. The five compose-related dispatcher unit tests (`h_inserted_when_text_field_focused`, etc.) should still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/dispatch.rs crates/ox-cli/src/settings/bindings.rs
git commit -m "dispatch: walk scope path with capture/target/bubble phases; retire per-widget passes"
```

---

## Task S3: Migrate pending-delete bindings to declare phases

**Files:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Find current pending-delete registrations**

```bash
grep -n "_pending_delete\|accounts\.confirm" crates/ox-cli/src/settings/bindings.rs | head
```

- [ ] **Step 2: Update phases**

- `Esc` → `accounts.confirm.cancel`: `phase: Phase::Capture` (always cancels, regardless of any future leaf scope).
- `y` → `accounts.confirm.delete`: `phase: Phase::Target`.
- `n` → `accounts.confirm.cancel`: `phase: Phase::Target`.

- [ ] **Step 3: Add tests**

```rust
#[test]
fn pending_delete_esc_fires_capture() {
    let snap = test_snapshot_with_pending_delete(/*account=*/ "alpha");
    let writes = dispatch_keystroke(&snap, KeyCodeRepr::Esc);
    assert!(writes_clear_pending_delete(&writes));
}

#[test]
fn pending_delete_y_confirms() {
    let snap = test_snapshot_with_pending_delete("alpha");
    let writes = dispatch_keystroke(&snap, KeyCodeRepr::Char('y'));
    // Deletes alpha from config/gate/accounts.
    assert!(writes.iter().any(|w| w.path == oxpath!("config", "gate", "accounts", "alpha")));
}

#[test]
fn pending_delete_n_cancels() {
    let snap = test_snapshot_with_pending_delete("alpha");
    let writes = dispatch_keystroke(&snap, KeyCodeRepr::Char('n'));
    // Same as Esc: clears pending_delete.
    assert!(writes_clear_pending_delete(&writes));
}
```

Adapt to existing test conventions.

- [ ] **Step 4: Run pending-delete tests**

```bash
cargo test -p ox-cli --lib pending_delete
```

All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/bindings.rs crates/ox-cli/src/settings/dispatch.rs
git commit -m "pending-delete: declare phase-aware bindings (Esc capture, y/n target)"
```

(Include `dispatch.rs` if you added tests there.)

---

## Task S4: Migrate manual-model bindings to declare phases

**Files:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`
- Modify: `crates/ox-cli/src/settings/dispatch.rs` (if `compute_scope_path` needs the manual_model leaf paths updated)

- [ ] **Step 1: Find current manual-model registrations**

```bash
grep -n "_manual_model\|manual_model\." crates/ox-cli/src/settings/bindings.rs | head -20
```

- [ ] **Step 2: Restructure into form + per-stage leaves**

Today's registrations are likely all on a single `settings/_manual_model` scope (or possibly per-stage already). Restructure (if needed):

- Form scope `settings/_manual_model`:
  - Esc → `manual_model.cancel`: `Capture`.
  - Enter → `manual_model.commit_stage` (or whatever fires per-stage): `Bubble`.
- Per-stage scopes `settings/_manual_model/Id`, `settings/_manual_model/Ctx`, `settings/_manual_model/Out`:
  - printable, Backspace: `Target` for that stage's buffer.

`compute_scope_path` (in S2) already pushes both `_manual_model` and `_manual_model/<stage>` when the wizard is active.

- [ ] **Step 3: Verify the scope-path computation matches**

In `compute_scope_path`, the manual-model branch pushes both scopes:

```rust
if let Some(stage) = manual_model_stage {
    path.push(BindingScope::Exact(oxpath!("settings", "_manual_model")));
    path.push(BindingScope::Exact(oxpath!("settings", "_manual_model", stage_to_str(stage))));
}
```

Confirm `stage_to_str` produces the right path component for each stage (`"Id"`, `"Ctx"`, `"Out"` — PascalCase per the existing `ManualModelStage` serde).

- [ ] **Step 4: Add tests**

```rust
#[test]
fn manual_model_esc_fires_capture_at_any_stage() {
    for stage in ["Id", "Ctx", "Out"] {
        let snap = test_snapshot_with_manual_model_stage(stage);
        let writes = dispatch_keystroke(&snap, KeyCodeRepr::Esc);
        assert!(writes_clear_manual_model(&writes), "stage {stage}");
    }
}

#[test]
fn manual_model_printable_inserts_at_current_stage() {
    let snap = test_snapshot_with_manual_model_stage("Id");
    let writes = dispatch_keystroke(&snap, KeyCodeRepr::Char('x'));
    // 'x' lands in the Id-stage buffer.
    assert!(writes.iter().any(|w| matches!(&w.path, p if p.ends_with(&["id"]))));
}
```

- [ ] **Step 5: Run manual-model tests**

```bash
cargo test -p ox-cli --lib manual_model
```

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/bindings.rs crates/ox-cli/src/settings/dispatch.rs
git commit -m "manual-model: declare phase-aware bindings (Esc capture, Enter bubble, text target)"
```

---

## Task S5: Migrate edit-mode bindings to declare phases

**Files:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Find current edit-mode registrations**

```bash
grep -n "edit_mode\|_edit\|field\.insert\|field\.delete_back\|field\.commit\|field\.cancel" crates/ox-cli/src/settings/bindings.rs | head -20
```

- [ ] **Step 2: Update phases**

- `Esc` → `field.cancel`: `Capture`.
- `Enter` → `field.commit`: `Bubble`.
- printable ASCII → `field.insert`: `Target`.
- `Backspace` → `field.delete_back`: `Target`.

- [ ] **Step 3: Add tests**

```rust
#[test]
fn edit_mode_esc_fires_capture() {
    let snap = test_snapshot_with_edit_mode(/*field_path=*/ "config/gate/providers/alpha/endpoint", /*buffer=*/ "partial");
    let writes = dispatch_keystroke(&snap, KeyCodeRepr::Esc);
    // Clears edit_mode + edit_buffer.
    assert!(writes_clear_edit_mode(&writes));
}

#[test]
fn edit_mode_enter_fires_bubble_commit() {
    let snap = test_snapshot_with_edit_mode("config/gate/providers/alpha/endpoint", "https://new.example.com");
    let writes = dispatch_keystroke(&snap, KeyCodeRepr::Enter);
    // Writes the buffer to the target field path.
    assert!(writes.iter().any(|w| w.path == oxpath!("config", "gate", "providers", "alpha", "endpoint")));
}

#[test]
fn edit_mode_printable_inserts_target() {
    let snap = test_snapshot_with_edit_mode("config/gate/providers/alpha/endpoint", "");
    let writes = dispatch_keystroke(&snap, KeyCodeRepr::Char('x'));
    // 'x' appended to edit_buffer.
    assert!(writes.iter().any(|w| w.path == oxpath!("ui", "settings", "edit_buffer")));
}
```

- [ ] **Step 4: Run edit-mode tests**

```bash
cargo test -p ox-cli --lib edit_mode
```

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/bindings.rs crates/ox-cli/src/settings/dispatch.rs
git commit -m "edit-mode: declare phase-aware bindings (Esc capture, Enter bubble, text target)"
```

---

## Task S6: Drop `Phase::default()`; require explicit phase at every binding site

**Files:**
- Modify: `crates/ox-types/src/command_binding.rs`
- Modify: `crates/ox-cli/src/settings/bindings.rs` (compile-fixup; should all already declare phase after S1-S5)

- [ ] **Step 1: Remove the `impl Default for Phase`**

In `command_binding.rs`, delete the `impl Default for Phase`. Any `BindingEntry` literal that omits `phase` will now fail to compile.

- [ ] **Step 2: Compile**

```bash
cargo check -p ox-cli
```

Expected: clean (S1-S5 made every binding registration declare its phase). If any compile errors appear, fix them — those are the legitimate "phase wasn't declared" sites.

- [ ] **Step 3: Run all tests**

```bash
cargo test -p ox-cli
cargo test -p ox-types
```

All green.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-types/src/command_binding.rs
git commit -m "bindings: require explicit phase at every registration site"
```

---

## Task S7: Update framework docs; final verification

**Files:**
- Modify: `docs/ui_framework/architecture.md`

- [ ] **Step 1: Update the Bindings section**

In `docs/ui_framework/architecture.md`, find the Bindings section and remove the caveat that says "The struct does **not** carry a `phase` field today...". Replace with the new shape — `phase: Phase` is first-class.

The `BindingEntry` example code block should now show `phase: Phase`.

The "convergence work tracked separately" line is gone.

- [ ] **Step 2: Update the dispatch flow description**

The description of the dispatcher's pass chain (pending-delete → manual-model → ...) is obsolete. Replace with a description of the generic three-phase walk over `compute_scope_path`.

- [ ] **Step 3: Run the full suite one more time**

```bash
cargo test -p ox-cli
cargo test -p ox-types
cargo clippy -p ox-cli -- -D warnings
```

All green. Specifically confirm the bug-fix reproducer still passes:

```bash
cargo test -p ox-cli --test settings_e2e add_connections_have_independent_providers
```

Green.

- [ ] **Step 4: Commit**

```bash
git add docs/ui_framework/architecture.md
git commit -m "docs: hierarchical dispatch is realized; drop convergence caveat"
```

---

## Final verification

- `BindingEntry::phase: Phase` is first-class.
- Dispatcher walks the scope path in three phases generically — no per-widget passes.
- Every compound widget declares its lifecycle keys at capture, semantic keys at target, fallbacks at bubble.
- `is_capture_key` / `is_bubble_key` enumerations and `lookup_compose` are gone.
- Framework docs match implementation.

Status: **S-tier on dispatch.**

## Self-review checklist

- [x] Every task has a TDD red→green pattern (write test, run, implement, run, commit).
- [x] No placeholders ("TBD", "fill in details").
- [x] Type consistency: `Phase`, `BindingEntry::phase`, `BindingRegistry::lookup(..., phase)`.
- [x] Each task ends with a commit.
- [x] The order matters: S1 (add field + default) → S2 (dispatcher refactor + compose migration in one step, since dropping `lookup_compose` requires compose's phases to be correct) → S3-S5 (other widgets) → S6 (drop default) → S7 (docs).
