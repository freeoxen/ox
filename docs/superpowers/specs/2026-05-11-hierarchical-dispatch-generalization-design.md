# Hierarchical dispatch generalization

**Date:** 2026-05-11
**Status:** Design — pending implementation plan
**Crates touched:** `ox-types` (BindingEntry), `ox-cli` (dispatcher + all settings-screen bindings), framework docs
**Spec context:** Closes out the framework's fourth commitment (hierarchical dispatch with capture/target/bubble phases) by making `Phase` a first-class field on `BindingEntry` and migrating every compound widget on the settings screen onto the phase-aware model. Today only compose mode exercises capture/bubble, via dispatcher-pass-local `is_capture_key`/`is_bubble_key` enumerations — a structural smell.

## 1. Summary

Today's binding model is flat: every `BindingEntry` is implicitly "target phase" — fired when its scope is the focused leaf. The hierarchical-dispatch commitment (added to `ui_framework.md` during the compose-as-whole-form work) describes a three-phase dispatch model (Capture → Target → Bubble) but the implementation realizes it only for compose, via hardcoded key-classification helpers in `dispatch.rs::lookup_compose`.

This design promotes `Phase` to a first-class field on `BindingEntry`. Every binding declares its phase at registration. The dispatcher walks the scope path in three phases generically — no per-widget pass logic, no key-classification enumerations. Every compound widget (compose, pending-delete, manual-model, edit-mode) declares its lifecycle keys at capture, semantic keys at target, fallbacks at bubble.

## 2. Goals & non-goals

### Goals

- `BindingEntry::phase: Phase` as a registry-level field.
- `BindingRegistry::lookup` phase-aware.
- Dispatcher walks the scope path in three phases: capture (outer→inner) → target (leaf only) → bubble (inner→outer). First match wins; phase boundary stops the walk.
- Scope path computed once per keystroke from snapshot state. No mutable scope-stack.
- All compound widgets migrate: compose, pending-delete, manual-model, edit-mode.
- `is_capture_key` / `is_bubble_key` enumerations and `lookup_compose` retire.
- Framework docs drop their "convergence work tracked separately" caveat — the model is realized.

### Non-goals

- Refactoring the existing top-level `dispatch_settings_key` shape beyond what the phase walk requires.
- Changing how cursor scopes map to pages (`settings`, `settings/accounts`, etc.).
- Generalizing further: a full "component tree" with sibling resolution, dynamic scope insertion, etc. The current shape (linear path, one active leaf) is enough.
- Migrating non-settings-screen dispatch (inbox, threads — currently use the legacy input-store path).

## 3. The model

A keystroke routes through a **scope path** — the ordered chain from outermost (the screen) to innermost (the focused leaf). Each scope on the path can claim the keystroke in one of three phases:

1. **Capture** (outermost → innermost): each scope's `Capture`-phase bindings are consulted. First match fires; dispatch ends.
2. **Target** (leaf only): the focused leaf's `Target`-phase bindings are consulted.
3. **Bubble** (innermost → outermost): each scope's `Bubble`-phase bindings are consulted. First match fires.

If no binding matches in any phase, dispatch falls through to legacy input-store handling.

### `BindingEntry` shape

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Capture,
    Target,
    Bubble,
}

pub struct BindingEntry {
    pub screen:      Screen,
    pub cursor_path: Option<Path>,
    pub mode:        Option<Mode>,
    pub key:         KeyChord,
    pub command_id:  CommandId,
    pub phase:       Phase,
}
```

### Lookup signature

```rust
impl BindingRegistry {
    pub fn lookup(
        &self,
        screen: Screen,
        cursor: &Path,
        mode: Option<&Mode>,
        key: &KeyChord,
        phase: Phase,
    ) -> Option<&CommandId>;
}
```

Specificity tie-breaking within a phase: same as today (`cursor_path: Some + mode: Some` > `cursor_path: Some + mode: None` > etc.).

### Scope path computation

`compute_scope_path(snap, cursor) -> Vec<Path>` reads UI-state discriminators and assembles the outer-to-inner path:

1. Outermost: the cursor scope (page scope, e.g., `settings/accounts`).
2. Active compound widget (at most one — modes are mutually exclusive):
   - `pending_delete: Some(_)` → `settings/_pending_delete`.
   - `manual_model/stage: Some(stage)` → `settings/_manual_model` + `settings/_manual_model/<stage>`.
   - `new_account/active == true` → `settings/_compose_form` + `settings/_compose_field_{text,selector}` (per kind of focused field).
   - `edit_mode + edit_field_path` set → `settings/_edit_mode` + `settings/_edit_field` (per kind, if needed).
3. The cursor itself is the innermost when no compound widget is active.

The deepest scope on the path is the **leaf** for target-phase dispatch.

### Dispatcher main loop

```rust
fn dispatch_settings_key(...) -> Option<Vec<Write>> {
    let scope_path = compute_scope_path(snap, cursor);

    // Capture: outer → inner
    for scope in &scope_path {
        if let Some(cmd) = bindings.lookup(screen, scope, mode, key, Phase::Capture) {
            return Some(run(cmd));
        }
    }

    // Target: leaf only
    if let Some(leaf) = scope_path.last() {
        if let Some(cmd) = bindings.lookup(screen, leaf, mode, key, Phase::Target) {
            return Some(run(cmd));
        }
    }

    // Bubble: inner → outer
    for scope in scope_path.iter().rev() {
        if let Some(cmd) = bindings.lookup(screen, scope, mode, key, Phase::Bubble) {
            return Some(run(cmd));
        }
    }

    None
}
```

Per-widget passes (`lookup_compose`, `read_pending_delete`-driven pass, etc.) retire. The compound-widget knowledge moves into `compute_scope_path`.

## 4. Per-widget phase assignments

### Compose mode (already exists)

- Form scope `settings/_compose_form`:
  - Capture: `Esc` → `accounts.compose.cancel`, `Tab`/`Down` → `accounts.compose.focus_next`, `Shift+Tab`/`Up` → `accounts.compose.focus_prev`.
  - Bubble: `Enter` → `accounts.compose.commit`.
- Field-text scope `settings/_compose_field_text`:
  - Target: printable ASCII → `accounts.compose.insert_char`; `Backspace` → `accounts.compose.delete_back`.
- Field-selector scope `settings/_compose_field_selector`:
  - Target: `h`/`Left` → `accounts.compose.cycle_back`; `l`/`Right` → `accounts.compose.cycle_forward`.

### Pending-delete confirmation

- Scope `settings/_pending_delete`:
  - Capture: `Esc` → `accounts.confirm.cancel`.
  - Target: `y` → `accounts.confirm.delete`, `n` → `accounts.confirm.cancel`.

### Manual-model wizard

- Form scope `settings/_manual_model`:
  - Capture: `Esc` → `manual_model.cancel`.
  - Bubble: `Enter` → `manual_model.commit_stage`.
- Per-stage scopes `settings/_manual_model/<Id|Ctx|Out>`:
  - Target: printable ASCII → `manual_model.insert_char`; `Backspace` → `manual_model.delete_back`.

### Edit mode (real-account inline field edit)

- Form scope `settings/_edit_mode`:
  - Capture: `Esc` → `field.cancel`.
  - Bubble: `Enter` → `field.commit`.
- Field scope `settings/_edit_field` (or per-kind):
  - Target: printable ASCII → `field.insert`; `Backspace` → `field.delete_back`.

(Exact command ids match what's already registered; this section just declares the phase assignments.)

### Cursor scope (no active compound widget)

- The cursor's own bindings (`j`/`k` to navigate rows, `a` to open compose, etc.) all register at Target on the cursor scope. The existing behavior is preserved.

## 5. Migration strategy

The migration must be order-sensitive: adding `phase` to `BindingEntry` is a breaking change to every binding registration. Strategy:

1. **Add `Phase` enum + `phase: Phase` to `BindingEntry`.** Add a `Default` impl returning `Phase::Target` so existing code that doesn't yet name a phase compiles. Update `BindingRegistry::lookup` to take a phase argument. Compose's `lookup_compose` migrates to query Capture, Target, Bubble explicitly via the new lookup signature — its hardcoded `is_capture_key`/`is_bubble_key` enumerations stay as the temporary classification. Behavior unchanged.

2. **Refactor the dispatcher's main loop.** Replace per-widget passes (`lookup_compose`, the pending-delete pass, the manual-model pass, the edit-mode pass) with a single generic three-phase walk over `compute_scope_path`. Drop `lookup_compose` and the key-classification enumerations. After this commit, compose-mode bindings need to declare their phases properly — they currently all sit at the Target default, which would break Esc/Tab/Enter behavior. So compose phase migration must land in this same commit or the next.

3. **Migrate compose bindings.** Esc/Tab/Up/Down register at Capture phase; Enter at Bubble; everything else at Target.

4. **Migrate pending-delete bindings.** Esc → Capture, y/n → Target.

5. **Migrate manual-model bindings.** Per-stage scopes; Esc → Capture; Enter → Bubble; printable/Backspace → Target.

6. **Migrate edit-mode bindings.** Esc → Capture; Enter → Bubble; printable/Backspace → Target.

7. **Cleanup.** Drop the `Phase::default() == Target` impl once every binding registration declares its phase explicitly. Update the framework docs to remove the "convergence work tracked separately" caveat.

Each step keeps the system green (full lib + e2e suites pass).

## 6. Risks

- **Scope-path drift.** If `compute_scope_path` misses an active compound widget, the dispatcher silently skips its bindings — no compile-time signal. Mitigation: explicit tests for each compound widget's scope-path shape; a registry round-trip test that asserts every registered scope is reachable from at least one `compute_scope_path` output.
- **Order-of-modes priority.** Today the dispatcher's pass chain is `pending-delete → manual-model → compose → edit-mode → focused-row → page-cursor`. The scope-path walk effectively inverts this (innermost first for target; outermost first for capture). If two modes are simultaneously active (shouldn't happen by design, but if it does), the inner one wins. Mitigation: keep the existing mutual-exclusion invariants; add a debug-assert that at most one compound-widget mode is active at a time.
- **Lookup performance.** Three lookups per keystroke (one per phase per scope on the path) vs today's one. The registry is a linear scan today; even a 5-deep scope path × 3 phases is 15 scans — still O(n) on a few-hundred-binding registry. Mitigation: none needed unless profiling shows it's hot.
- **Test coverage gap.** Some existing tests assert specific binding-firing order (e.g., the dispatcher's pass-chain priority). They need to be re-expressed in phase terms.

## 7. What stays the same

- `cursor_path` semantics. Scopes are still identified by a path.
- `Mode` discriminator. Per-cursor modes still exist (rare, but legitimate when a single scope wants to bind a key differently in different sub-states).
- The `command!` macro and command-registration pattern.
- All command bodies. No command's `run` closure changes shape.
- Subscription dispatch and effect handling. Unchanged.

## 8. Execution

The implementation plan is at `docs/superpowers/plans/2026-05-11-hierarchical-dispatch-generalization.md`. ~7 commits, each landing one step from §5 above. Subagent-driven.

After landing:
- `BindingEntry::phase` is a first-class field.
- The dispatcher's main loop is one phase-walk, no per-widget passes.
- Every compound widget on the settings screen declares its phases explicitly.
- The framework's fourth commitment is fully realized in code.
