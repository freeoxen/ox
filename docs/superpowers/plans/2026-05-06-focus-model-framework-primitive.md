# Phase 0: Focus model framework primitive — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the focus-model framework primitive — `FocusId` newtype, `ListItem.focus: Option<FocusId>`, `View::focus_enumeration()`, and rename `ui/settings/focused_row: Path` to `ui/settings/focused: Option<Path>` — so subsequent substrate phases can express decorations as `focus: None` ListItems.

**Architecture:** Three additions to ox-view (newtype, field, walker method) plus a CLI-side snap-based `focus_enumeration` helper that mirrors `visible_rows::enumerate` but returns `FocusId`s. The dispatcher's j/k logic switches from `position_of(visible_rows, focused_row)` to walking the snap-based focus enumeration. The View's `focus_enumeration` method is added for future renderer-driven dispatch but is not consumed by the dispatcher in this phase — the snap-based helper and the View method return structurally equivalent results today (both derived from `visible_rows::enumerate`).

**Tech Stack:** Rust workspace; `ox-view` (View enum), `ox-cli` (renderers, dispatcher, commands, tests), `ox-types` (subscription path types unchanged).

**Spec:** [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md) §4.5 + Phase 0.

---

## File Map

**Modify:**
- `crates/ox-view/src/lib.rs` — add `FocusId` newtype, add `focus` field to `ListItem`, add `View::focus_enumeration()` method, update internal tests' `ListItem` constructions.
- `crates/ox-cli/src/view_render.rs` — update `ListItem` constructions in tests; the translator itself doesn't read `focus`.
- `crates/ox-cli/src/settings/renderers/index.rs` — set `focus: Some(FocusId(row.path.clone()))` on every emitted `ListItem`.
- `crates/ox-cli/src/settings/visible_rows.rs` — add a `focus_enumeration_from_snap` helper that returns `Vec<FocusId>` mirroring `enumerate`'s row order.
- `crates/ox-cli/src/settings/commands/tree.rs` — `step` (j/k), `activate`, `jump`, and `read_focused` switch to the new `ui/settings/focused` path; `step` and `jump` use the new snap-based focus helper.
- `crates/ox-cli/src/settings/commands/edit.rs` — wherever it reads/writes `focused_row` (e.g. for begin-edit + commit cascades), update path string.
- `crates/ox-cli/src/settings/commands/account_model.rs` — same.
- `crates/ox-cli/src/settings/commands/navigation.rs` — same if applicable.
- `crates/ox-cli/src/settings/renderers/index.rs::read_cursor` (the focused-row reader) — update path string.
- `crates/ox-gate/src/subscriptions/account_create.rs` — the subscription writes `focused_row` after creating an account; update path.
- `crates/ox-cli/tests/settings_e2e.rs` — `h.focused_row()` helper or any direct `oxpath!("ui","settings","focused_row")` reads/writes.
- The `E2eHarness` (probably in `crates/ox-cli/tests/`) — if it has a `focused_row()` accessor, rename to `focused()` and update its read path.

**Create:**
- (none — all additions land in existing files)

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section comments in code. Doc comments explaining WHY are fine.
- Tests-first where new behavior is being added; mechanical migrations don't need new tests when existing coverage is sufficient (call this out per task).

---

## Task 1: Add `FocusId` newtype to `ox-view`

**Files:**
- Modify: `crates/ox-view/src/lib.rs` (add the type near the other sub-types, around line 80–95).

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/ox-view/src/lib.rs`:

```rust
#[test]
fn focus_id_is_a_path_newtype_with_value_equality() {
    use structfs_core_store::Path;
    let p1 = Path::parse("settings/accounts/alpha").unwrap();
    let p2 = Path::parse("settings/accounts/alpha").unwrap();
    let p3 = Path::parse("settings/accounts/beta").unwrap();
    assert_eq!(FocusId(p1.clone()), FocusId(p2));
    assert_ne!(FocusId(p1), FocusId(p3));
}
```

- [ ] **Step 2: Run the test; expect failure**

```
cargo test -p ox-view --lib focus_id_is_a_path_newtype
```

Expected: FAIL — `FocusId` not defined.

- [ ] **Step 3: Add the type**

In `crates/ox-view/src/lib.rs`, after the `ListItem` struct definition (around line 92), add:

```rust
/// Identity of a focusable widget. The dispatcher's keyboard
/// navigation (`j`/`k`) walks the focus enumeration of the current
/// View; the focused widget's identity is stored at
/// `ui/settings/focused` in the broker (as the underlying `Path`).
///
/// `FocusId` wraps a `Path` rather than aliasing it so that function
/// signatures distinguish "this is a focus identity" from "this is a
/// data-tree path." On the wire (in the broker) the value is just
/// the inner `Path`; the wrapping is for type safety at the CLI
/// dispatch boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FocusId(pub structfs_core_store::Path);
```

- [ ] **Step 4: Run the test; expect pass**

```
cargo test -p ox-view --lib focus_id_is_a_path_newtype
```

Expected: PASS.

- [ ] **Step 5: Run the full ox-view tests**

```
cargo test -p ox-view --lib
```

Expected: PASS.

- [ ] **Step 6: Commit**

```
git add crates/ox-view/src/lib.rs
git commit -m "feat(view): add FocusId newtype for typed focus identity

Wraps a Path so function signatures distinguish focus identities
from arbitrary data-tree paths. The wire shape (in the broker) is
the inner Path; the wrapping is CLI-side type safety only. No
serde dependency added — ox-view stays minimal."
```

---

## Task 2: Add `focus: Option<FocusId>` field to `ListItem`

This task adds the field as REQUIRED (no default). Every existing `ListItem` construction site in the workspace must be updated in the same commit. The compiler enforces completeness — `cargo build -p ox-cli` won't pass until every site is migrated.

**Files:**
- Modify: `crates/ox-view/src/lib.rs` (the `ListItem` struct + 6 internal test constructions at lines 333, 339, 350, 356, 371, 464).
- Modify: `crates/ox-cli/src/view_render.rs` (3 test constructions at lines 494, 500, 562).
- Modify: `crates/ox-cli/src/settings/renderers/index.rs` (1 production construction at line 76 + 1 at line 85; both emit ListItems for real rows).

- [ ] **Step 1: Add the field to the struct**

In `crates/ox-view/src/lib.rs`, modify the `ListItem` struct (around lines 81–91):

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub primary: String,
    /// When present, the translator renders this styled-span sequence
    /// in place of `primary`. Used by the settings tree to render
    /// inline selector carousels — `[prev_dim] current [next_dim]` —
    /// without inventing a new `View` variant.
    pub primary_spans: Option<Vec<Span>>,
    pub secondary: Option<String>,
    pub badge: Option<String>,
    /// Focus identity. `Some(FocusId(path))` marks this item as a
    /// navigation target — the dispatcher's `j`/`k` cycles through
    /// items with `focus: Some(...)` only. `None` marks the item as
    /// a non-navigable decoration (banner, affordance, header).
    pub focus: Option<FocusId>,
}
```

- [ ] **Step 2: Verify the crate fails to compile**

```
cargo build -p ox-view
```

Expected: FAIL with errors about `ListItem` constructions in the test module missing the `focus` field.

- [ ] **Step 3: Update ox-view's internal test ListItem constructions**

In `crates/ox-view/src/lib.rs`'s `#[cfg(test)] mod tests`, every `ListItem { ... }` literal needs the `focus` field added. There are six (around lines 333, 339, 350, 356, 371, 464). For test-only items, the focus value doesn't affect what's being tested — set `focus: None` everywhere in the lib.rs tests.

Example shape per site:

```rust
ListItem {
    primary: "alpha".into(),
    primary_spans: None,
    secondary: None,
    badge: None,
    focus: None,
}
```

Touch all six sites.

- [ ] **Step 4: Run ox-view tests**

```
cargo test -p ox-view --lib
```

Expected: PASS.

- [ ] **Step 5: Update view_render.rs test ListItem constructions**

In `crates/ox-cli/src/view_render.rs`, the test ListItems at lines 494, 500, 562 each need `focus: None`. Same shape as Step 3.

- [ ] **Step 6: Update the renderer's production ListItem constructions**

In `crates/ox-cli/src/settings/renderers/index.rs`, the two ListItem constructions (around line 76 and line 85) emit items derived from `visible_rows::enumerate`. Each derives from a `row: &VisibleRow` whose `row.path: Path` is the natural focus identity.

For both sites:

```rust
ListItem {
    primary: format!("{indent}{glyph}{}", row.label),
    primary_spans: Some(spans),  // or None depending on which arm
    secondary: row.secondary.clone(),
    badge: row.badge.clone(),
    focus: Some(ox_view::FocusId(row.path.clone())),
}
```

Add `use ox_view::FocusId;` to the file's imports if it isn't already importing from `ox_view::*`.

- [ ] **Step 7: Build the workspace; expect green**

```
cargo build --workspace
```

Expected: PASS. Any compile error names a missed ListItem construction; add `focus: None` (for test-only items) or `focus: Some(FocusId(...))` (for renderer-emitted items) per the rules above.

- [ ] **Step 8: Run the full ox-cli lib tests**

```
cargo test -p ox-cli --lib
```

Expected: PASS. The ListItem field addition doesn't affect existing test expectations — every existing assertion on rendered output checks primary/secondary/badge values, not focus.

- [ ] **Step 9: Commit**

```
git add -u
git commit -m "feat(view): add ListItem.focus field for typed focus identity

Every ListItem now declares whether it's a navigation target
(focus: Some) or a decoration (focus: None). Existing call sites
get focus: Some for real rows derived from visible_rows
(production); focus: None for test-only items where focus value
isn't under test. The dispatcher will start consuming this in a
later task."
```

---

## Task 3: Add `View::focus_enumeration()` method

This is the framework primitive walker. It returns the focusable items in display order. For Phase 0, the dispatcher uses a snap-based helper instead (Task 4) — this method exists for future use when a renderer emits navigable items not derived from `visible_rows::enumerate`.

**Files:**
- Modify: `crates/ox-view/src/lib.rs` (add an `impl View { fn focus_enumeration ... }` block).

- [ ] **Step 1: Write failing tests**

Add to `crates/ox-view/src/lib.rs`'s test module:

```rust
#[test]
fn focus_enumeration_empty_for_view_without_focusables() {
    let view = View::Text { spans: vec![Span::plain("hi")], align: Align::Left };
    assert!(view.focus_enumeration().is_empty());
}

#[test]
fn focus_enumeration_collects_list_items_in_order() {
    use structfs_core_store::Path;
    let view = View::List {
        items: vec![
            ListItem {
                primary: "alpha".into(),
                primary_spans: None,
                secondary: None,
                badge: None,
                focus: Some(FocusId(Path::parse("a").unwrap())),
            },
            ListItem {
                primary: "decoration".into(),
                primary_spans: None,
                secondary: None,
                badge: None,
                focus: None,  // skipped
            },
            ListItem {
                primary: "beta".into(),
                primary_spans: None,
                secondary: None,
                badge: None,
                focus: Some(FocusId(Path::parse("b").unwrap())),
            },
        ],
        selected: None,
    };
    let ids = view.focus_enumeration();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], FocusId(Path::parse("a").unwrap()));
    assert_eq!(ids[1], FocusId(Path::parse("b").unwrap()));
}

#[test]
fn focus_enumeration_descends_into_stack_and_pad_and_modal() {
    use structfs_core_store::Path;
    let make_list_with_one = |id: &str| View::List {
        items: vec![ListItem {
            primary: id.into(),
            primary_spans: None,
            secondary: None,
            badge: None,
            focus: Some(FocusId(Path::parse(id).unwrap())),
        }],
        selected: None,
    };
    let stack = View::Stack {
        dir: Direction::Vertical,
        children: vec![
            (make_list_with_one("a"), Sizing::Fill),
            (make_list_with_one("b"), Sizing::Fill),
        ],
    };
    let padded = View::Pad {
        padding: Padding { top: 0, right: 0, bottom: 0, left: 0 },
        child: Box::new(stack),
    };
    let modal = View::Modal {
        background: Box::new(make_list_with_one("bg")),
        foreground: Box::new(padded),
        dim: true,
    };
    let ids = modal.focus_enumeration();
    // Both background and foreground contribute. Background first
    // (it's drawn first); foreground after.
    assert_eq!(
        ids,
        vec![
            FocusId(Path::parse("bg").unwrap()),
            FocusId(Path::parse("a").unwrap()),
            FocusId(Path::parse("b").unwrap()),
        ]
    );
}
```

- [ ] **Step 2: Run; expect failure**

```
cargo test -p ox-view --lib focus_enumeration
```

Expected: FAIL — `focus_enumeration` method does not exist on `View`.

- [ ] **Step 3: Implement the method**

Add to `crates/ox-view/src/lib.rs`, after the `View` enum definition (or anywhere a free `impl View` block fits):

```rust
impl View {
    /// Walk the View tree and collect every focusable widget's
    /// `FocusId` in display order. The dispatcher uses this to
    /// determine `j`/`k` traversal targets.
    ///
    /// Decorations (items with `focus: None`, banners, status
    /// blocks, etc.) are skipped. Composite widgets (`Stack`,
    /// `Modal`, `Pad`, `Frame`) recurse into their children.
    pub fn focus_enumeration(&self) -> Vec<FocusId> {
        let mut out = Vec::new();
        self.collect_focus_into(&mut out);
        out
    }

    fn collect_focus_into(&self, out: &mut Vec<FocusId>) {
        match self {
            View::Empty
            | View::Text { .. }
            | View::Form { .. }
            | View::Banner { .. }
            | View::StatusBlock { .. } => {}
            View::List { items, .. } => {
                for item in items {
                    if let Some(id) = &item.focus {
                        out.push(id.clone());
                    }
                }
            }
            View::Stack { children, .. } => {
                for (child, _) in children {
                    child.collect_focus_into(out);
                }
            }
            View::Modal { background, foreground, .. } => {
                background.collect_focus_into(out);
                foreground.collect_focus_into(out);
            }
            View::Pad { child, .. } => {
                child.collect_focus_into(out);
            }
            View::Frame { content, .. } => {
                content.collect_focus_into(out);
            }
        }
    }
}
```

(If the `View` enum's variant set differs in name from what's listed here — e.g., a variant has been added since this plan was drafted — the match must remain exhaustive. Add the missing variant to the match with the appropriate behavior: composites recurse, leaves are no-ops, list-shaped variants extract from items.)

- [ ] **Step 4: Run the new tests; expect pass**

```
cargo test -p ox-view --lib focus_enumeration
```

Expected: PASS for all three.

- [ ] **Step 5: Run full ox-view tests**

```
cargo test -p ox-view --lib
```

Expected: PASS.

- [ ] **Step 6: Commit**

```
git add crates/ox-view/src/lib.rs
git commit -m "feat(view): View::focus_enumeration() walks the tree for focusables

Collects every ListItem with focus: Some(...) in display order,
descending through Stack / Modal / Pad / Frame composites. The
dispatcher will consume this in future framework work; for now it
exists alongside a snap-based helper that returns the same
information for the current renderer set."
```

---

## Task 4: Add the snap-based `focus_enumeration` helper

The dispatcher needs a way to get the focus enumeration without rendering the View. For Phase 0, the helper mirrors `visible_rows::enumerate` (every visible row is focusable) and returns the same `FocusId`s the renderer would tag. When future renderers emit navigable items not derived from visible_rows, this helper grows; for now it's a one-line projection.

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs` (add the helper at the end, near `position_of`).

- [ ] **Step 1: Write a failing test**

Add to `crates/ox-cli/src/settings/visible_rows.rs`'s test module:

```rust
#[test]
fn focus_enumeration_mirrors_visible_rows_paths() {
    let mut snap = SettingsSnapshot::empty();
    write_index_entries(&mut snap);
    write_account(&mut snap, "alpha");
    write_account(&mut snap, "beta");
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );

    let rows = enumerate(&mut snap);
    let focus_ids = focus_enumeration(&mut snap);

    assert_eq!(focus_ids.len(), rows.len());
    for (row, id) in rows.iter().zip(focus_ids.iter()) {
        assert_eq!(id.0, row.path);
    }
}
```

- [ ] **Step 2: Run; expect failure**

```
cargo test -p ox-cli --lib settings::visible_rows::tests::focus_enumeration_mirrors_visible_rows_paths
```

Expected: FAIL — `focus_enumeration` helper not defined.

- [ ] **Step 3: Implement the helper**

Add to `crates/ox-cli/src/settings/visible_rows.rs`, near `position_of`:

```rust
/// Snap-based focus enumeration. Returns the `FocusId` of every
/// navigable widget in the current settings view, in display order.
///
/// For Phase 0 (the focus model framework primitive), every visible
/// row from `enumerate` is a focusable target; the helper is a
/// one-line projection. Future phases that introduce decorations
/// (renderer-emitted items not in `enumerate`) leave this helper
/// unchanged — decorations have `focus: None` in the renderer's
/// output and don't appear in this enumeration either.
pub fn focus_enumeration(data: &mut dyn Reader) -> Vec<ox_view::FocusId> {
    enumerate(data)
        .into_iter()
        .map(|row| ox_view::FocusId(row.path))
        .collect()
}
```

- [ ] **Step 4: Run the test; expect pass**

```
cargo test -p ox-cli --lib settings::visible_rows::tests::focus_enumeration_mirrors_visible_rows_paths
```

Expected: PASS.

- [ ] **Step 5: Run the full visible_rows test module**

```
cargo test -p ox-cli --lib settings::visible_rows::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```
git add crates/ox-cli/src/settings/visible_rows.rs
git commit -m "feat(settings): snap-based focus_enumeration helper

Mirrors visible_rows::enumerate for navigation purposes. Every
visible row is currently a focusable target; the helper exists so
the dispatcher can express j/k traversal as 'walk the focus
enumeration' instead of 'walk visible_rows.' Future decorations
(focus: None ListItems) leave this helper unchanged — they only
exist in the renderer's output, never in visible_rows or the focus
enumeration."
```

---

## Task 5: Rename `ui/settings/focused_row` → `ui/settings/focused`

Mechanical find-replace across the workspace. The wire shape stays the same (Path or absent); only the path string changes. Read/write helpers may also need their names updated for consistency.

**Files (all read/written in this task):**
- `crates/ox-cli/src/settings/commands/tree.rs`
- `crates/ox-cli/src/settings/commands/edit.rs`
- `crates/ox-cli/src/settings/commands/account_model.rs`
- `crates/ox-cli/src/settings/commands/navigation.rs`
- `crates/ox-cli/src/settings/renderers/index.rs`
- `crates/ox-cli/src/settings/snapshot.rs` (the `fetch_settings_view_state` walks `ui/settings`; verify the renamed path is still pulled in)
- `crates/ox-gate/src/subscriptions/account_create.rs`
- `crates/ox-cli/tests/settings_e2e.rs`
- The `E2eHarness` (`crates/ox-cli/tests/harness.rs` or wherever `h.focused_row()` is defined)
- Any other file the inventory grep below surfaces.

- [ ] **Step 1: Inventory the references**

Run:

```
grep -rn 'focused_row' crates/ tests/ 2>/dev/null
```

Capture the full hit list. Expected: 80–90 hits across ~10 files.

- [ ] **Step 2: Do the rename**

For every hit, replace:
- `oxpath!("ui", "settings", "focused_row")` → `oxpath!("ui", "settings", "focused")`
- The string literal `"ui/settings/focused_row"` → `"ui/settings/focused"`
- Helper function names: `read_focused()` stays (it already reads "the focused thing"); `focused_row()` accessor on the e2e harness becomes `focused()`. Function bodies use the new path.
- Test assertion strings comparing path shapes (`assert_eq!(write.path.to_string(), "ui/settings/focused_row")`) → update to the new string.

Where the path is constructed via `oxpath!`, use `"focused"` as the final component. Where it's compared as a stringified Path (e.g., `path.to_string()`), use `"ui/settings/focused"`.

Use a careful global search-and-replace tool, then visually verify the diff with `git diff` before staging. Any reference in a `#`-comment or doc comment should also update for consistency.

- [ ] **Step 3: Verify the workspace compiles**

```
cargo build --workspace
```

Expected: PASS. Any compile error means a reference was missed; grep again and fix.

- [ ] **Step 4: Run the lib test suite**

```
cargo test -p ox-cli --lib
cargo test -p ox-gate --lib
```

Expected: PASS for both. Any test failure that depends on the old path name needs its assertion updated to the new path.

- [ ] **Step 5: Run the e2e tests**

```
cargo test -p ox-cli --test settings_e2e
```

Expected: PASS. The `E2eHarness::focused()` accessor (if renamed) needs callers updated.

- [ ] **Step 6: Verify clippy is clean**

```
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 7: Final grep — confirm no stragglers**

```
grep -rn 'focused_row' crates/ tests/ 2>/dev/null
```

Expected: zero hits.

- [ ] **Step 8: Commit**

```
git add -u
git commit -m "refactor(settings): rename ui/settings/focused_row → ui/settings/focused

Mechanical path rename across ~10 files. The wire shape is unchanged
(Path or absent); only the path string changes. The semantic shift
(from 'focused row in visible_rows' to 'focused widget identity, a
FocusId') is conceptual; the next task wires the dispatcher to walk
the focus enumeration instead of visible_rows directly."
```

---

## Task 6: Switch dispatcher j/k from `visible_rows` to focus_enumeration

Update `tree::step` and `tree::jump` to use the snap-based `focus_enumeration` helper. `tree::activate` continues to read `focused` and dispatch on the underlying Path (the FocusId.0 is the row's display path, same path-equality dispatch as today).

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/tree.rs` — `step`, `jump`, `read_focused`. Possibly `activate` if it changes shape.

- [ ] **Step 1: Read the current `step`, `jump`, and `read_focused` implementations**

Read `crates/ox-cli/src/settings/commands/tree.rs` lines 100–155 to anchor the current shapes. The current `step` uses `visible_rows::enumerate(data)` + `visible_rows::position_of(&rows, c)`; `jump` uses `visible_rows::enumerate(data)` + indexes by first/last. Both write the row's path to `ui/settings/focused`.

- [ ] **Step 2: Update `read_focused` doc and body (no behavior change)**

In `crates/ox-cli/src/settings/commands/tree.rs`, the `read_focused` function (around line 123 today):

```rust
/// Read the focused-widget identity. This is intentionally NOT
/// `ui/settings/cursor`: cursor identifies the active page (the
/// renderer + binding-scope), which on the accordion screen is always
/// `settings/index`. The focused widget inside that page lives at
/// `ui/settings/focused`. Conflating the two breaks binding dispatch
/// (the binding lookup uses cursor as its scope key).
///
/// Returns the focused widget's underlying `Path` (the inner value
/// of the conceptual `FocusId` — see `ox_view::FocusId`).
fn read_focused(data: &mut dyn Reader) -> Option<structfs_core_store::Path> {
    let record = data
        .read(&oxpath!("ui", "settings", "focused"))
        .ok()
        .flatten()?;
    let value = record.as_value()?;
    path_from_value(value)
}
```

(This is functionally identical to the post-Task-5 state; the change is the doc comment honesty.)

- [ ] **Step 3: Update `step` to walk focus_enumeration**

Replace the existing `step` (around lines 132–151) with:

```rust
fn step(data: &mut dyn Reader, direction: Direction) -> Vec<Write> {
    let focus_ids = visible_rows::focus_enumeration(data);
    if focus_ids.is_empty() {
        return Vec::new();
    }
    let current = read_focused(data);
    let current_idx = current
        .as_ref()
        .and_then(|p| focus_ids.iter().position(|f| &f.0 == p))
        .unwrap_or(0);
    let next_idx = match direction {
        Direction::Next => (current_idx + 1) % focus_ids.len(),
        Direction::Prev => (current_idx + focus_ids.len() - 1) % focus_ids.len(),
    };
    let target = &focus_ids[next_idx].0;
    vec![Write {
        path: oxpath!("ui", "settings", "focused"),
        record: Record::parsed(path_to_value(target)),
    }]
}
```

The semantic equivalence to the old code is exact: `focus_enumeration` mirrors `visible_rows::enumerate` and the underlying Paths are identical. The structural change is that navigation is now keyed on focus identity, not row identity.

- [ ] **Step 4: Update `jump` similarly**

Replace the existing `jump` (around lines 102–115) with:

```rust
fn jump(data: &mut dyn Reader, to: JumpTo) -> Vec<Write> {
    let focus_ids = visible_rows::focus_enumeration(data);
    if focus_ids.is_empty() {
        return Vec::new();
    }
    let target = match to {
        JumpTo::First => &focus_ids[0].0,
        JumpTo::Last => &focus_ids[focus_ids.len() - 1].0,
    };
    vec![Write {
        path: oxpath!("ui", "settings", "focused"),
        record: Record::parsed(path_to_value(target)),
    }]
}
```

- [ ] **Step 5: Verify `activate` doesn't need a structural change**

Read `crates/ox-cli/src/settings/commands/tree.rs::activate` (around line 153). It does `visible_rows::enumerate(data); position_of(rows, &cursor)` to find the focused row, then dispatches by `RowKind`. This continues to work because:
- `cursor` (read via `read_focused`) returns the underlying Path of the focused FocusId.
- That Path equals the corresponding row's `path` in `visible_rows::enumerate` (by construction of focus_enumeration).
- `position_of` finds the matching row.

No change needed to `activate`. Confirm by reading and continuing.

- [ ] **Step 6: Run the tree test module**

```
cargo test -p ox-cli --lib settings::commands::tree::tests
```

Expected: PASS. The behavior is unchanged; only the data flow (focus_enumeration vs. visible_rows) differs, and they produce identical results.

- [ ] **Step 7: Run the full ox-cli lib + e2e**

```
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
```

Expected: PASS.

- [ ] **Step 8: Commit**

```
git add crates/ox-cli/src/settings/commands/tree.rs
git commit -m "refactor(settings): tree.step + tree.jump walk focus_enumeration

Navigation now sources its targets from
visible_rows::focus_enumeration rather than visible_rows::enumerate
directly. Behavior is identical today (the helper mirrors
enumerate); the structural change is that navigation is keyed on
focus identity (FocusId) rather than row identity (RowKind path).
This unlocks Phase 3+ where renderer-emitted decorations have
focus: None and are naturally skipped by traversal.

read_focused's doc comment is updated to describe what it actually
reads now: the focused-widget identity (a path) at the
ui/settings/focused path. tree.activate continues to dispatch by
path-equality against visible_rows; the Paths involved are
identical."
```

---

## Task 7: Final verification

Confirm the workspace is fully green before declaring Phase 0 complete.

- [ ] **Step 1: Full workspace test run**

```
cargo test --workspace
```

Expected: PASS — every crate's tests are green.

- [ ] **Step 2: Clippy on every target**

```
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS — no warnings.

- [ ] **Step 3: Verify no stragglers**

```
grep -rn 'focused_row' crates/ tests/ docs/ 2>/dev/null
```

Expected: hits ONLY in markdown documents (the spec, the framework docs, this plan) referencing the historical name. No source-code or test-code hits.

- [ ] **Step 4: Confirm e2e test count is unchanged**

```
cargo test -p ox-cli --test settings_e2e 2>&1 | grep "test result"
```

Expected: same number of tests as before Phase 0 began. Phase 0 doesn't change test counts; it changes the data shape under the test assertions.

- [ ] **Step 5: Smoke-test in the TUI**

The harness can't run the interactive TUI. Ask the user to:

1. Open settings (whatever key opens it).
2. Press `j` and `k` repeatedly to confirm navigation through the accordion still works.
3. Press `Enter` to expand/collapse and confirm activation still works.
4. Press `Tab` (or however the screen-exit shortcut is bound) to leave settings and re-enter; confirm the focused-row state persists or resets per existing behavior (Phase 0 doesn't change the persistence shape).

If anything misbehaves, that's a regression — investigate before declaring Phase 0 complete.

---

## Self-review checklist

After all tasks land, verify against the spec's §4.5 + Phase 0:

- [x] `FocusId(Path)` newtype added to ox-view (Task 1).
- [x] `ListItem.focus: Option<FocusId>` field added (Task 2).
- [x] `View::focus_enumeration()` method added, walks the View tree (Task 3).
- [x] Snap-based `focus_enumeration` helper added (Task 4).
- [x] `ui/settings/focused_row` renamed to `ui/settings/focused` (Task 5).
- [x] Dispatcher j/k uses focus_enumeration (Task 6).
- [x] Workspace green (Task 7).

Spec requirements not addressed by this plan (intentionally deferred):
- View-driven dispatch (the spec's "view.focus_enumeration().position(...)" language). Phase 0 uses the snap-based helper; the View method is added for future use. The decision is documented in the spec's §4.5 ("the snap-based helper and the View method return structurally equivalent results today") and reiterated in Task 4's commit message and helper doc. Future framework work can switch the dispatcher to View-driven when a renderer emits navigable items not derived from visible_rows.

No placeholders. No "similar to Task N." Every code step shows the actual code. Every command has its expected output.
