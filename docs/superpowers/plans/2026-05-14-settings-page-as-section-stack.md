# Settings Page as Section-Stack Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development.

**Goal:** Refactor the settings IndexRenderer to emit the page as `Frame → Stack[AccountsSection, ModelsSection]` instead of one flat `Frame → List`. Compose form lives in the Accounts section's middle slot by construction, eliminating the positioning bug and the decoration-insertion offset math.

**Spec:** `docs/superpowers/specs/2026-05-14-settings-page-as-section-stack-design.md`.

---

## Task SS-1: Refactor IndexRenderer to emit Stack-of-Sections

**Files:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs` (the `render` method + helpers)
- Modify: `crates/ox-cli/src/settings/renderers/index.rs` test module (view-tree assertions)
- Possibly modify: `crates/ox-cli/tests/snapshots/` (re-accept any byte-level snapshot diffs)

**Step 1: Write the failing view-tree test**

Add to the test module in `index.rs`:

```rust
#[test]
fn page_emits_frame_stack_of_two_sections() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    let view = render(&mut snap);

    let stack_children = match view {
        View::Frame { content, .. } => match *content {
            View::Stack { dir: Direction::Vertical, children } => children,
            other => panic!("expected Stack inside Frame, got {other:?}"),
        },
        other => panic!("expected Frame, got {other:?}"),
    };
    assert_eq!(stack_children.len(), 2, "page is two sections (Accounts, Models)");
    // First child is AccountsSection (Stack), second is ModelsSection (Stack).
    assert!(matches!(stack_children[0].0, View::Stack { .. }));
    assert!(matches!(stack_children[1].0, View::Stack { .. }));
}

#[test]
fn accounts_section_has_header_only_when_collapsed_and_compose_inactive() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    // No expand, no compose.
    let view = render(&mut snap);
    let accounts_section = extract_accounts_section(view);

    // Just the header — no middle, no content.
    match accounts_section {
        View::Stack { children, .. } => {
            assert_eq!(children.len(), 1);
            assert!(matches!(children[0].0, View::List { .. }));
        }
        _ => panic!("AccountsSection should be a Stack"),
    }
}

#[test]
fn accounts_section_adds_affordance_when_expanded_and_compose_inactive() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    write_account(&mut snap, "alpha");

    let view = render(&mut snap);
    let accounts_section = extract_accounts_section(view);

    let children = match accounts_section {
        View::Stack { children, .. } => children,
        _ => panic!("AccountsSection should be a Stack"),
    };
    // Header + affordance + content list.
    assert_eq!(children.len(), 3);
    // Middle slot is a List with the affordance.
    let middle_items = match &children[1].0 {
        View::List { items, .. } => items,
        _ => panic!("middle slot should be a List of affordance"),
    };
    assert_eq!(middle_items.len(), 1);
    assert!(middle_items[0].primary.contains("+ New connection"));
}

#[test]
fn accounts_section_shows_compose_form_in_middle_when_active() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "active"),
        Value::Bool(true),
    );
    // (Plus the minimum compose state to render the form — focused_field, etc.)

    let view = render(&mut snap);
    let accounts_section = extract_accounts_section(view);

    let children = match accounts_section {
        View::Stack { children, .. } => children,
        _ => panic!("AccountsSection should be a Stack"),
    };
    // Header + Form. (Content list absent when Accounts is collapsed.)
    // Or Header + Form + Content list when Accounts is also expanded.
    assert!(children.len() >= 2);
    let form_child_present = children.iter().any(|(v, _)| matches!(v, View::Form { .. }));
    assert!(form_child_present, "compose Form must be inside Accounts section");
}

#[test]
fn models_section_holds_empty_catalog_decorations_inside_its_content() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    write_account(&mut snap, "alpha"); // empty catalog
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/models".to_string()]),
    );

    let view = render(&mut snap);
    let models_section = extract_models_section(view);
    let children = match models_section {
        View::Stack { children, .. } => children,
        _ => panic!("ModelsSection should be a Stack"),
    };
    assert!(children.len() >= 2);
    // Content list (children[1]) holds the empty-state decoration + manual-add affordance for alpha.
    let content_items = match &children[1].0 {
        View::List { items, .. } => items,
        _ => panic!("Models content should be a List"),
    };
    assert!(content_items.iter().any(|it| it.primary.contains("no models")));
    assert!(content_items.iter().any(|it| it.primary.contains("add model manually")));
}

#[test]
fn focused_path_selects_in_matching_sub_list_only() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    write_account(&mut snap, "alpha");
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    // Cursor on the alpha row.
    snap.insert(
        &oxpath!("ui", "settings", "focused"),
        path_to_value(&oxpath!("settings", "accounts", "alpha")),
    );

    let view = render(&mut snap);
    let accounts_section = extract_accounts_section(view);
    let children = match accounts_section {
        View::Stack { children, .. } => children,
        _ => panic!("AccountsSection should be a Stack"),
    };

    // The header sub-List has selected: None (cursor isn't on the header).
    if let View::List { selected, .. } = &children[0].0 {
        assert!(selected.is_none(), "header should not be selected");
    }
    // The content sub-List has selected: Some(0) (alpha is the only row).
    let content_idx = children.len() - 1;
    if let View::List { selected, .. } = &children[content_idx].0 {
        assert_eq!(*selected, Some(0), "content list selects alpha row");
    }
}
```

Helpers `extract_accounts_section(view)` and `extract_models_section(view)` walk `Frame → Stack → children` and return the appropriate child View. Define them in the test module.

Adapt fixture names (`write_index`, `write_account`, `expanded_set_to_value`, `path_to_value`) to what already exists in the test module.

**Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib page_emits_frame_stack accounts_section models_section focused_path_selects
```

Expected: FAIL. The current renderer emits `Frame → List` (or `Frame → Stack[Form, List]` when compose active) — neither matches the new structure.

**Step 3: Rewrite `IndexRenderer::render`**

Replace the existing `render` method. Outline:

```rust
fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
    let rows = visible_rows::enumerate(ctx.data);
    let edit_state = read_edit_state(ctx.data);
    let cursor = read_cursor(ctx.data);
    let compose_active = read_compose_active(ctx.data);

    // Partition rows into per-section row groups by Entry boundary.
    let (accounts_rows, models_rows) = partition_rows_by_section(&rows);

    // Resolve selector option lists once for the focused row.
    let (protocol_options, auth_current) = resolve_focused_selector_state(ctx.data, &rows, &cursor);

    // Each section is its own Stack.
    let accounts_section = render_accounts_section(
        ctx.data,
        accounts_rows,
        &cursor,
        edit_state.as_ref(),
        compose_active,
        &protocol_options,
        auth_current.as_ref(),
    );
    let models_section = render_models_section(
        ctx.data,
        models_rows,
        &cursor,
        edit_state.as_ref(),
        &protocol_options,
        auth_current.as_ref(),
    );

    let (title, title_right) = compute_frame_chrome(ctx.data);

    View::Frame {
        title,
        title_right,
        content: Box::new(View::Stack {
            dir: Direction::Vertical,
            children: vec![
                (accounts_section, Sizing::Min(0)),
                (models_section, Sizing::Min(0)),
            ],
        }),
    }
}
```

Then implement the helpers:

```rust
fn partition_rows_by_section(rows: &[VisibleRow]) -> (Vec<&VisibleRow>, Vec<&VisibleRow>) {
    // Walk rows; rows up through the Models Entry boundary go to Accounts;
    // Models Entry and everything after go to Models.
    // (More sections in the future would generalize to a multi-way partition.)
    let models_pos = rows.iter().position(|r|
        matches!(&r.kind, RowKind::Entry { entry_id } if entry_id == "models")
    );
    match models_pos {
        Some(m) => (rows[..m].iter().collect(), rows[m..].iter().collect()),
        None => (rows.iter().collect(), vec![]),
    }
}

fn render_accounts_section(
    data: &mut dyn Reader,
    rows: Vec<&VisibleRow>,
    cursor: &Option<Path>,
    edit_state: Option<&EditState>,
    compose_active: bool,
    protocol_options: &[String],
    auth_current: Option<&AuthScheme>,
) -> View {
    // First row should be the Accounts Entry. Build header sub-list from it.
    // Remaining rows are account rows and their expanded field rows (when applicable).
    let mut children: Vec<(View, Sizing)> = Vec::new();

    let (header_row, content_rows) = rows.split_first().expect("Accounts section has at least the header row");
    let header_expanded = header_row.expanded;

    // Header sub-List (always present).
    let header_list = build_list_from_rows(
        std::slice::from_ref(header_row),
        cursor,
        edit_state,
        protocol_options,
        auth_current,
    );
    children.push((header_list, Sizing::Fixed(1)));

    // Middle slot.
    if compose_active {
        let form = compose_form_view(data);
        let form_height = form_view_height(&form);
        children.push((form, Sizing::Min(form_height)));
    } else if header_expanded {
        let affordance_item = ListItem {
            primary: "    + New connection".to_string(),
            primary_spans: None,
            secondary: None,
            badge: None,
            focus: None,
        };
        let affordance_list = View::List {
            items: vec![affordance_item],
            selected: None, // focus: None means it never matches the cursor
        };
        children.push((affordance_list, Sizing::Fixed(1)));
    }

    // Content sub-List (account rows + their field rows when expanded).
    if header_expanded && !content_rows.is_empty() {
        let content_list = build_list_from_rows(
            content_rows,
            cursor,
            edit_state,
            protocol_options,
            auth_current,
        );
        children.push((content_list, Sizing::Min(0)));
    }

    View::Stack { dir: Direction::Vertical, children }
}

fn render_models_section(
    data: &mut dyn Reader,
    rows: Vec<&VisibleRow>,
    cursor: &Option<Path>,
    edit_state: Option<&EditState>,
    protocol_options: &[String],
    auth_current: Option<&AuthScheme>,
) -> View {
    if rows.is_empty() {
        // No Models entry at all (degenerate config). Return Empty.
        return View::Empty;
    }
    let mut children: Vec<(View, Sizing)> = Vec::new();

    let (header_row, content_rows) = rows.split_first().expect("Models section has at least the header row");
    let header_expanded = header_row.expanded;

    let header_list = build_list_from_rows(
        std::slice::from_ref(header_row),
        cursor,
        edit_state,
        protocol_options,
        auth_current,
    );
    children.push((header_list, Sizing::Fixed(1)));

    if header_expanded {
        // Content rows from the projection.
        let mut content_items = rows_to_list_items(content_rows, cursor, edit_state, protocol_options, auth_current);

        // Interleave empty-catalog decorations (the existing logic, but now operating
        // on this section's local item vector — no rows-index vs items-index divergence).
        interleave_empty_catalog_decorations(data, &mut content_items, &content_rows);

        let content_selected = content_items.iter().position(|it|
            it.focus.as_ref().map(|f| Some(&f.0) == cursor.as_ref()).unwrap_or(false)
        );

        children.push((
            View::List { items: content_items, selected: content_selected },
            Sizing::Min(0),
        ));
    }

    View::Stack { dir: Direction::Vertical, children }
}
```

The helper `build_list_from_rows` does the row → ListItem conversion (currently inline in the existing `render` method's `.map(...)` block). Extract it.

`interleave_empty_catalog_decorations` does the existing empty-state decoration insertion, but now operating on items WITHIN the Models section's content list. No `models_idx`-from-rows-applied-to-items confusion: both `content_rows` and `content_items` are in the Models section local space.

`form_view_height(view)` computes the form's row count from its `View::Form` shape. Reuse the existing helper.

**Step 4: Run to verify the new tests pass**

```bash
cargo test -p ox-cli --lib page_emits_frame_stack accounts_section models_section focused_path_selects
```

Expected: all pass.

**Step 5: Run the full suite — fix any structural-assertion test regressions**

```bash
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
```

Expected breakage: tests that asserted the old `Frame → List` shape (e.g., `index_renderer_emits_frame_list_when_compose_inactive` from T13). Update them to assert the new `Frame → Stack[Section, Section]` shape OR delete them if they're now redundant with the new section-structure tests.

The byte-level insta snapshots SHOULD still pass — the rendered TUI output for typical layouts is the same row sequence. If a snapshot fails:
- Inspect the diff carefully.
- If it's a positioning improvement (compose form now appears in the right place, decorations correctly inside Models section), accept the new snapshot.
- If it's an unexpected byte change, debug.

**Step 6: Verify the compose form positioning is fixed**

Add a test that drives the dispatcher to open compose, and asserts the form's rendered output appears INSIDE the Accounts section (not before the Connections header):

```rust
#[test]
fn compose_form_renders_below_accounts_header_in_section() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "active"),
        Value::Bool(true),
    );
    // (Plus minimum compose state.)

    let view = render(&mut snap);

    // Flatten the rendered output to a string and assert order: the Connections
    // header comes BEFORE the form. Use a simple flatten helper that walks
    // Frame → Stack → (Section Stack) → List items in order.
    let flat = flatten_to_strings(&view);
    let header_pos = flat.iter().position(|s| s.contains("Connections")).expect("Connections header present");
    let form_first_field = flat.iter().position(|s| s.contains("Name:")).expect("compose form Name field present");
    assert!(header_pos < form_first_field, "Connections header must render BEFORE compose form fields");
}
```

`flatten_to_strings` walks the View tree and collects `primary` strings from Lists + labels from Form rows, in render order.

**Step 7: Verify the Models-decoration bug is structurally fixed**

The yesterday bug (decoration before Models header) is now impossible by construction — the empty-catalog decorations live INSIDE the Models section's content List. But pin it with a test:

```rust
#[test]
fn empty_catalog_decoration_renders_inside_models_section_after_header() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    write_account(&mut snap, "aaa");  // empty catalog
    write_account_with_models(&mut snap, "bbb", &["m1"]);
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&[
            "settings/accounts".to_string(),
            "settings/models".to_string(),
        ]),
    );

    let view = render(&mut snap);
    let flat = flatten_to_strings(&view);
    let header_pos = flat.iter().position(|s| s.contains("Models")).expect("Models header present");
    let decoration_pos = flat.iter().position(|s| s.contains("no models")).expect("decoration present for aaa");
    assert!(header_pos < decoration_pos, "Models header must render BEFORE empty-catalog decoration");
}
```

(The previous fix added this test against `items`; this version reasserts it against the new View tree structure.)

**Step 8: Cleanup**

Remove the now-dead helpers:
- `find_accounts_header_followup_idx`
- Whatever helper computed `models_header_idx` against rows (now inlined into the Models section renderer).
- The `selected.map(|s| if s >= insert_idx { s + N } else { s })` bookkeeping — gone, each sub-List has its own `selected`.

If `compose_form_view` and `form_height` were placed in `account_model.rs` (T13's choice), they can stay there. The IndexRenderer just calls them.

**Step 9: Commit**

```bash
git add crates/ox-cli/src/settings/renderers/index.rs crates/ox-cli/tests/snapshots/
git commit -m "render: settings page as Stack of Sections; compose form lives inside Accounts section"
```

Stage only:
- `crates/ox-cli/src/settings/renderers/index.rs`
- Any updated snapshot files under `crates/ox-cli/tests/snapshots/`

Do NOT stage the untracked plan/spec markdown files.

---

## Self-review checklist

- [ ] Page is `Frame → Stack[AccountsSection, ModelsSection]`.
- [ ] AccountsSection is a Stack with header + optional middle (form or affordance) + optional content list.
- [ ] ModelsSection is a Stack with header + optional content list (with empty-catalog decorations interleaved).
- [ ] No new View enum variants. No new ListItem fields.
- [ ] `find_accounts_header_followup_idx` is gone.
- [ ] Insertion-offset math (`models_idx + 1` from rows-into-items) is gone.
- [ ] Selection (`ui/settings/focused`) still works — each sub-List computes its own selected.
- [ ] j/k navigation unchanged (operates on visible_rows projection).
- [ ] Compose form renders INSIDE the Accounts section (asserted by test).
- [ ] Models empty-catalog decorations render INSIDE the Models section (asserted by test).
- [ ] All 5+ new structural tests pass.
- [ ] Existing e2e snapshot tests pass (or are updated for the new structure with clear visual improvement).
- [ ] Full lib + e2e suite green.
- [ ] Reproducer (`add_connections_have_independent_providers`) green.
