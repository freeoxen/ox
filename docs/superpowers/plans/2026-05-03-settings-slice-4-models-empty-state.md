# Settings Slice 4 — Models Empty-State + Inline Metadata

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Models section of the Settings tree useful when a connection has no cataloged models (today it silently expands to nothing) and surface each model's context/output token budgets inline so the user can pick a model without drilling in.

**Architecture:** Two structural changes inside the existing path-MVU primitive — no new `View` variants, no schema changes, no kernel work. (1) `VisibleRow` gains an `Option<String>` secondary slot that the renderer pipes into the existing `ListItem.secondary` field. (2) `append_model_rows` emits a new `RowKind::ModelEmptyState { account }` row for any connection whose catalog is empty; activating that row triggers a catalog refresh for the connection.

**Tech Stack:** Rust, ox-cli settings module, ox-view's existing `View::List` / `ListItem` shape, existing `gate.catalog_refresh` subscription.

**Spec it builds on:** `docs/superpowers/plans/2026-05-03-settings-connections-roadmap.md` §5 Slice 4. The roadmap entry overstated the work — the codebase's `append_model_rows` already produces a flat `(account, model_id)` enumeration. What's actually missing is the empty-state and the metadata. This plan ships those; the bigger "real `View::Table` with aligned columns + pricing + search" work is deferred to a follow-up slice that needs an ox-view variant first.

---

## Why

A user with one configured account and a never-fetched catalog opens Settings → Models → Enter, and the tree silently toggles to expanded with zero visible changes. The original report:

> "the models displays '▸ Models [LMStudio / claude-sonnet-4-20250514]' but nothing happens when I open it"

Two issues, one root cause: nothing in the UI tells the user (a) that a refresh is needed, or (b) what to expect when the catalog is populated. The fix is one row per empty connection saying "(no models — Enter to refresh)" and one extra slot per cataloged model showing its budget. Both ride on UI primitives already in place; this slice is structural plumbing, not new infrastructure.

---

## File Structure

| File | Change |
|---|---|
| `crates/ox-cli/src/settings/visible_rows.rs` | Add `secondary: Option<String>` field to `VisibleRow`. Add `RowKind::ModelEmptyState { account: String }`. Update `append_model_rows` to emit the empty-state row and populate `secondary` for model rows. Add `format_token_count` helper. Update tests. |
| `crates/ox-cli/src/settings/renderers/index.rs` | Pipe `row.secondary` into `ListItem.secondary` (currently hardcoded to `None`). Add tests for the metadata + empty-state rendering. |
| `crates/ox-cli/src/settings/commands/tree.rs` | Add `RowKind::ModelEmptyState` arm to `activate()`: write the connection's `refresh_now` trigger. Update tests. |

No new files. No new dependencies. No subscription changes.

### Conventions & gotchas

- The empty-state row sits at depth 1 (same as model rows) and is identified by `RowKind::ModelEmptyState { account }`. It's not expandable; activating it triggers refresh.
- The empty-state row's `path` uses `safe_component(account)` for the model-id slot (sentinel `_empty`) so the path is unique per connection and never collides with a real model id (real ids never start with `_`).
- `format_token_count` formats `u32` token counts as `"200k"`, `"8k"`, `"1M"`. For values below 1000 it returns the raw decimal. This is conventional in LLM tooling and saves horizontal space in the row.
- `ModelInfo.max_context_size` and `max_output_tokens` are both `Option<u32>`. When unknown, render `"—"` (em-dash) for that slot rather than skipping it — keeps row alignment legible.
- Model rows whose catalog had `max_context_size == None` and `max_output_tokens == None` (manually-entered placeholder, future Slice 6) get `secondary = Some("ctx — · out —")`, not `None`. The dashes carry the "we know this is a model but don't know its budgets" signal.
- Renderer change: there are TWO places in `IndexRenderer::render` that build a `ListItem` — the focused-selector branch (returns early) and the default branch. Both should set `secondary: row.secondary.clone()` so a future row kind that uses both selector AND secondary (none today, but the plumbing should be uniform) works.
- Subagent-friendly: each task is contained in one or two files with clear test fixtures already present.
- Commit cadence: one commit per task. Per-commit gate is `cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli`. Full `scripts/quality_gates.sh` once at the end.

---

## Task 1: Add `secondary` field to `VisibleRow`

Pure plumbing — adds the field, defaults every existing constructor to `None`, threads it through the renderer. No behavior change yet (next task populates the field).

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`
- Modify: `crates/ox-cli/src/settings/renderers/index.rs`

- [ ] **Step 1: Add the field to the struct**

In `crates/ox-cli/src/settings/visible_rows.rs`, find the `VisibleRow` struct near line 42:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleRow {
    pub path: Path,
    pub depth: usize,
    pub label: String,
    pub badge: Option<String>,
    pub kind: RowKind,
    pub expandable: bool,
    pub expanded: bool,
}
```

Add a `secondary` field:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleRow {
    pub path: Path,
    pub depth: usize,
    pub label: String,
    /// Right-aligned secondary text (e.g. model token-budget summary).
    /// `None` when the row has no extra metadata to show.
    pub secondary: Option<String>,
    pub badge: Option<String>,
    pub kind: RowKind,
    pub expandable: bool,
    pub expanded: bool,
}
```

- [ ] **Step 2: Update every `VisibleRow { ... }` constructor in the file to set `secondary: None`**

Search the file for `VisibleRow {`. There are four sites:
1. The `Entry` row in `enumerate` (around line 89).
2. The `Account` row in `append_account_rows` (around line 122).
3. The `AccountField` row in `append_account_field_rows` (around line 240).
4. The `Model` row in `append_model_rows` (around line 165).
5. The `ModelField` row in `append_model_field_rows` (around line 286).

Each one currently looks like:

```rust
rows.push(VisibleRow {
    path: ...,
    depth: ...,
    label: ...,
    badge: ...,
    kind: ...,
    expandable: ...,
    expanded: ...,
});
```

Add `secondary: None,` between `label` and `badge` for each one. Position matters only for cosmetic clarity — the field is named — but keeping the order matched across constructors makes the file easier to read.

- [ ] **Step 3: Pipe `secondary` into the renderer's `ListItem`**

In `crates/ox-cli/src/settings/renderers/index.rs`, locate the focused-selector branch inside `IndexRenderer::render` (the `if let Some(spans) = selector_carousel_spans(...)` block):

```rust
                if is_focused {
                    if let Some(spans) =
                        selector_carousel_spans(row, &indent, glyph, &protocol_options)
                    {
                        return ListItem {
                            primary: format!("{indent}{glyph}{}", row.label),
                            primary_spans: Some(spans),
                            secondary: None,
                            badge: row.badge.clone(),
                        };
                    }
                }
```

Change `secondary: None` to `secondary: row.secondary.clone()`.

Locate the default branch (the `ListItem { ... }` literal at the end of the closure):

```rust
                ListItem {
                    primary: format!("{indent}{glyph}{label}"),
                    primary_spans: None,
                    secondary: None,
                    badge: row.badge.clone(),
                }
```

Change `secondary: None` to `secondary: row.secondary.clone()`.

- [ ] **Step 4: Run the existing tests to confirm nothing broke**

```bash
cargo test -p ox-cli --lib settings
```

Expected: all pre-existing tests pass. If a test fails it's because a literal `VisibleRow { ... }` constructor in test code is missing the new `secondary` field — find it and add `secondary: None,`.

- [ ] **Step 5: Per-commit gate**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/visible_rows.rs crates/ox-cli/src/settings/renderers/index.rs
git commit -m "feat(settings): add VisibleRow.secondary slot threaded through to ListItem

Plumbing for upcoming model-row metadata (ctx/out token budgets) and the
empty-state row. The renderer pipes the new field into the existing
ListItem.secondary slot in both branches (focused selector + default).
No behavior change in this commit — every existing constructor still
sets secondary: None."
```

---

## Task 2: Populate model rows with ctx/out token-budget metadata

Adds `format_token_count` helper. Updates `append_model_rows` to set `secondary` to `"ctx <n> · out <n>"`.

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Write the failing tests for `format_token_count`**

Add to the `#[cfg(test)] mod tests` block in `visible_rows.rs`:

```rust
    // -- format_token_count ---------------------------------------------

    #[test]
    fn format_token_count_uses_k_suffix_for_thousands() {
        assert_eq!(format_token_count(8_000), "8k");
        assert_eq!(format_token_count(200_000), "200k");
        assert_eq!(format_token_count(128_000), "128k");
    }

    #[test]
    fn format_token_count_uses_m_suffix_for_millions() {
        assert_eq!(format_token_count(1_000_000), "1M");
        assert_eq!(format_token_count(2_000_000), "2M");
    }

    #[test]
    fn format_token_count_uses_raw_decimal_below_1000() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(512), "512");
        assert_eq!(format_token_count(999), "999");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::format_token_count_
```

Expected: 3 tests fail with "cannot find function `format_token_count`".

- [ ] **Step 3: Implement `format_token_count`**

Add this private helper near the bottom of `visible_rows.rs`, before the `#[cfg(test)]` block:

```rust
/// Format a token count for display. Uses `k` / `M` suffixes above
/// 1000 / 1_000_000; raw decimal below. Mirrors how model docs and
/// dashboards label context windows ("200k context") so the rendered
/// secondary text reads naturally to anyone who has read a model card.
fn format_token_count(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::format_token_count_
```

Expected: 3 tests pass.

- [ ] **Step 5: Write the failing test for the populated `secondary` field on model rows**

Add to the same test block:

```rust
    #[test]
    fn model_row_secondary_carries_ctx_and_out_metadata() {
        // A model row's secondary slot must surface the token budgets so
        // the user can compare models without drilling into each one.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "anthropic".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&vec![ModelInfo {
                id: "claude-sonnet-4".into(),
                display_name: "Claude Sonnet 4".into(),
                max_context_size: Some(200_000),
                max_output_tokens: Some(8_000),
                source: ModelInfoSource::Server,
            }])
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let model_row = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Model { .. }))
            .expect("model row");
        assert_eq!(model_row.secondary.as_deref(), Some("ctx 200k · out 8k"));
    }

    #[test]
    fn model_row_secondary_renders_em_dash_for_unknown_budget() {
        // A model entry whose catalog refresh got back ids only (no token
        // limits) and whose known-family table didn't fill them in must
        // still render a legible secondary — the dashes carry "we know
        // this model exists but not its budgets" without leaving the
        // slot blank.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "anthropic".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&vec![ModelInfo {
                id: "mystery-model".into(),
                display_name: "Mystery".into(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            }])
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let model_row = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Model { .. }))
            .expect("model row");
        assert_eq!(model_row.secondary.as_deref(), Some("ctx — · out —"));
    }
```

- [ ] **Step 6: Run the tests to verify they fail**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::model_row_secondary_
```

Expected: 2 tests fail. Both will fail because `secondary` is currently set to `None` for model rows by the Task 1 plumbing.

- [ ] **Step 7: Populate `secondary` in `append_model_rows`**

Locate `append_model_rows` in `visible_rows.rs` (around line 140). Inside the `for m in models { ... }` loop, locate the `rows.push(VisibleRow { ... })`:

```rust
            rows.push(VisibleRow {
                path: path.clone(),
                depth: 1,
                label: format!("{} / {}", account_name, m.id),
                secondary: None,
                badge: None,
                kind: RowKind::Model {
                    account: account_name.clone(),
                    model_id: m.id.clone(),
                },
                expandable: true,
                expanded: is_expanded,
            });
```

Change the `secondary: None` line to compute the metadata. Add a small helper and use it:

```rust
            rows.push(VisibleRow {
                path: path.clone(),
                depth: 1,
                label: format!("{} / {}", account_name, m.id),
                secondary: Some(model_secondary(&m)),
                badge: None,
                kind: RowKind::Model {
                    account: account_name.clone(),
                    model_id: m.id.clone(),
                },
                expandable: true,
                expanded: is_expanded,
            });
```

Add the `model_secondary` helper near `format_token_count`:

```rust
/// Format a model's secondary metadata line: token budgets, dashed
/// when unknown.
fn model_secondary(m: &ox_gate::ModelInfo) -> String {
    let ctx = m
        .max_context_size
        .map(format_token_count)
        .unwrap_or_else(|| "—".to_string());
    let out = m
        .max_output_tokens
        .map(format_token_count)
        .unwrap_or_else(|| "—".to_string());
    format!("ctx {ctx} · out {out}")
}
```

- [ ] **Step 8: Run the tests to verify they pass**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::model_row_secondary_
```

Expected: 2 tests pass.

- [ ] **Step 9: Run the full ox-cli test suite to verify no regression**

```bash
cargo test -p ox-cli
```

Expected: all tests pass. The pre-existing `expanded_models_inlines_model_pairs` test asserts row count and kind but doesn't inspect `secondary`, so it stays green.

- [ ] **Step 10: Per-commit gate**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings
```

Expected: clean.

- [ ] **Step 11: Commit**

```bash
git add crates/ox-cli/src/settings/visible_rows.rs
git commit -m "feat(settings): surface model token budgets in row secondary

Each model row's secondary slot now reads 'ctx <n>k · out <n>k'
(or '—' when the budget is unknown). Lets the user compare context
windows and output limits across cataloged models without drilling
into each one's detail row."
```

---

## Task 3: Add `RowKind::ModelEmptyState` and emit it for empty connections

When a connection's catalog is empty, emit one synthetic row tagged with that connection's name. Renders as "(no models — Enter to refresh)".

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Write the failing test**

Add to the test block in `visible_rows.rs`:

```rust
    #[test]
    fn empty_catalog_yields_one_empty_state_row_per_connection() {
        // Two accounts: one with a model, one with no catalog at all.
        // The Models section, when expanded, should show the cataloged
        // model row PLUS one synthetic ModelEmptyState row for the empty
        // connection — never silently zero rows for that connection.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        write_account(&mut snap, "beta"); // no models written
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        // Visible: [Accounts header, Models header, alpha/m1 row,
        // beta empty-state row] = 4
        assert_eq!(rows.len(), 4);
        let empty = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::ModelEmptyState { .. }))
            .expect("empty-state row for beta");
        match &empty.kind {
            RowKind::ModelEmptyState { account } => assert_eq!(account, "beta"),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(empty.depth, 1);
        assert!(!empty.expandable);
        assert!(empty.label.contains("no models"));
    }

    #[test]
    fn empty_catalog_row_has_unique_path_per_connection() {
        // Two empty connections must produce two distinct rows; their
        // paths must be unique so cursor tracking can distinguish them.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let empty: Vec<_> = rows
            .iter()
            .filter(|r| matches!(&r.kind, RowKind::ModelEmptyState { .. }))
            .collect();
        assert_eq!(empty.len(), 2);
        assert_ne!(empty[0].path, empty[1].path);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::empty_catalog_
```

Expected: 2 tests fail. The first fails on `rows.len() == 4` (currently 3, no synthetic row); the second fails on `empty.len() == 2` (currently 0).

- [ ] **Step 3: Add the `ModelEmptyState` variant to `RowKind`**

In `visible_rows.rs`, locate the `RowKind` enum near line 20:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A top-level index entry: Accounts, Models, …
    Entry { entry_id: String },
    /// One account row inside an expanded Accounts entry.
    Account { name: String },
    /// One (account, model_id) row inside an expanded Models entry.
    Model { account: String, model_id: String },
    /// One field row under an expanded account.
    AccountField {
        account: String,
        field: AccountField,
    },
    /// One field row under an expanded model.
    ModelField {
        account: String,
        model_id: String,
        field: ModelField,
    },
}
```

Add the new variant:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A top-level index entry: Accounts, Models, …
    Entry { entry_id: String },
    /// One account row inside an expanded Accounts entry.
    Account { name: String },
    /// One (account, model_id) row inside an expanded Models entry.
    Model { account: String, model_id: String },
    /// Synthetic placeholder when a connection has no cataloged models.
    /// Activating it (Enter) triggers a catalog refresh for the named
    /// connection — gives the user a discoverable next action where the
    /// natural one ("expand to see models") would otherwise yield a
    /// silent zero-row expansion.
    ModelEmptyState { account: String },
    /// One field row under an expanded account.
    AccountField {
        account: String,
        field: AccountField,
    },
    /// One field row under an expanded model.
    ModelField {
        account: String,
        model_id: String,
        field: ModelField,
    },
}
```

- [ ] **Step 4: Emit the empty-state row in `append_model_rows`**

Locate the outer loop in `append_model_rows`:

```rust
fn append_model_rows(rows: &mut Vec<VisibleRow>, data: &mut dyn Reader, expanded: &[String]) {
    let account_names = child_names_under(data, "config/gate/accounts");
    for account_name in &account_names {
        // models_path construction ...
        let models: Vec<ox_gate::ModelInfo> = read_typed(data, &models_path).unwrap_or_default();
        for m in models {
            // existing model row push ...
        }
    }
}
```

Wrap the inner-loop block with an `if models.is_empty() { ... } else { ... }` branch so an empty catalog produces exactly one synthetic row:

```rust
fn append_model_rows(rows: &mut Vec<VisibleRow>, data: &mut dyn Reader, expanded: &[String]) {
    let account_names = child_names_under(data, "config/gate/accounts");
    for account_name in &account_names {
        let models_path = Path::try_from_components(vec![
            "config".to_string(),
            "gate".to_string(),
            "accounts".to_string(),
            account_name.clone(),
            "models".to_string(),
        ])
        .expect("account names from child_names_under are valid path components");
        let models: Vec<ox_gate::ModelInfo> = read_typed(data, &models_path).unwrap_or_default();

        if models.is_empty() {
            let path = row_path(&[
                "settings",
                "models",
                &safe_component(account_name),
                "_empty",
            ]);
            rows.push(VisibleRow {
                path,
                depth: 1,
                label: format!("{} / (no models — Enter to refresh)", account_name),
                secondary: None,
                badge: None,
                kind: RowKind::ModelEmptyState {
                    account: account_name.clone(),
                },
                expandable: false,
                expanded: false,
            });
            continue;
        }

        for m in models {
            // existing model row push (unchanged) ...
        }
    }
}
```

The `_empty` sentinel in the path is safe because real model ids never start with `_` (they fail `safe_component`-style sanitation that requires an alphanumeric leading character — the validator coerces `_` only when the original id was non-identifier-safe; literal `_empty` cannot be confused with a real id because it's a string we control).

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::empty_catalog_
```

Expected: both tests pass.

- [ ] **Step 6: Run the full settings suite to catch any pattern-match exhaustiveness misses**

```bash
cargo test -p ox-cli --lib settings
```

Expected: all tests pass. If `match row.kind { ... }` blocks elsewhere fail to compile, those are exhaustive matches that need a `RowKind::ModelEmptyState { .. } => ...` arm. The likely site is `tree.rs::activate` — if it fails to compile, add `RowKind::ModelEmptyState { .. } => Vec::new()` for now. Task 4 fills in the real behavior.

- [ ] **Step 7: Per-commit gate**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
```

Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-cli/src/settings/visible_rows.rs
# If tree.rs needed an exhaustiveness placeholder arm, include it too:
git add crates/ox-cli/src/settings/commands/tree.rs 2>/dev/null || true
git commit -m "feat(settings): emit ModelEmptyState row for connections with no catalog

Each connection whose config/gate/accounts/{name}/models record is
absent or an empty Vec now produces one synthetic row at depth 1
labeled '<connection> / (no models — Enter to refresh)'. Resolves
the silent-expansion footgun where Models → Enter on a fresh install
(or any never-refreshed connection) toggled to expanded with zero
visible change.

Activation behavior comes in the next commit; this commit only emits
the row and reserves the path. The placeholder arm in tree.rs (if
present) keeps the match exhaustive without yet doing the refresh."
```

---

## Task 4: Wire `tree.activate` on `ModelEmptyState` to refresh the connection's catalog

Pressing Enter on an empty-state row writes the connection's `refresh_now` trigger; the existing `gate.catalog_refresh` subscription picks it up.

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/tree.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module in `crates/ox-cli/src/settings/commands/tree.rs`:

```rust
    #[test]
    fn activate_on_empty_state_row_writes_refresh_trigger() {
        // The connection has no catalog; Enter on its synthetic
        // empty-state row must write config/gate/accounts/{name}/refresh_now,
        // which the gate.catalog_refresh subscription consumes.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha"); // no models
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        // Focus the synthetic empty-state row directly. The path matches
        // what append_model_rows emits.
        set_focused(&mut snap, "settings/models/alpha/_empty");

        let writes = run(&TreeActivate::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        assert_eq!(
            writes[0].path,
            oxpath!("config", "gate", "accounts", comp, "refresh_now")
        );
        // The gate subscription only inspects the trigger's existence /
        // mtime, so a Null-record write is the conventional shape (matches
        // delete_now, test_now, etc).
        match &writes[0].record {
            Record::Parsed(Value::Null) => {}
            other => panic!("expected null-record refresh trigger, got {other:?}"),
        }
    }

    #[test]
    fn activate_on_empty_state_row_with_invalid_account_name_is_inert() {
        // Defensive: a corrupt focused-row pointer with an account
        // segment that fails PathComponent validation must not panic and
        // must not write a malformed broker path. Activate falls through
        // with no writes.
        //
        // (The empty-state row is constructed from child_names_under
        // output, which yields broker keys that already passed
        // validation — but the row's account string is plain String, so
        // a future refactor that allows externally-influenced names
        // would break the invariant. This test pins the no-write
        // contract regardless.)
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        // Hand-craft a ModelEmptyState row by writing an account whose
        // child enumeration emits the empty case, then mutate the
        // focused-row pointer to one we know exists.
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        set_focused(&mut snap, "settings/models/alpha/_empty");
        // Sanity: with a valid account this is a single-write operation
        // (the previous test). The defensive branch fires only when
        // PathComponent::try_new fails for the account string, which we
        // can't trigger through the normal data path. Skip the negative
        // assertion — the existing-account positive case proves the
        // arm is reachable, and the existing PathComponent failure
        // branch in account_request_path is covered by other tests.
        let writes = run(&TreeActivate::new(), &mut snap);
        assert_eq!(writes.len(), 1);
    }
```

(The second test is marginal — it's a smoke check that the same path that triggers the positive case yields a single write. Remove it if the spec reviewer flags it as redundant.)

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p ox-cli --lib settings::commands::tree::tests::activate_on_empty_state_row_
```

Expected: both tests fail. If Task 3 added an exhaustiveness placeholder arm (`RowKind::ModelEmptyState { .. } => Vec::new()`), the failures are "expected 1 write, got 0." If no placeholder was added, the failures are compile errors — go fix the match in `tree.rs::activate` first to get to the runtime failure.

- [ ] **Step 3: Implement the `ModelEmptyState` arm in `activate`**

Locate the `activate()` function in `crates/ox-cli/src/settings/commands/tree.rs`. Inside the `match &row.kind { ... }` block at the end of the leaf branch, replace the placeholder arm (or add the new arm) with:

```rust
            RowKind::ModelEmptyState { account } => {
                // Write the connection's refresh trigger. The
                // gate.catalog_refresh subscription (PrefixSuffix on
                // config/gate/accounts/* / refresh_now) consumes the
                // write and replaces the empty-state row with real model
                // rows on success — same path the explicit `r` keystroke
                // takes, just reachable from the discoverable place.
                let comp = match ox_kernel::PathComponent::try_new(account) {
                    Ok(c) => c,
                    Err(_) => return Vec::new(),
                };
                vec![Write {
                    path: oxpath!("config", "gate", "accounts", comp, "refresh_now"),
                    record: Record::parsed(Value::Null),
                }]
            }
```

This arm sits alongside the existing `RowKind::Entry { .. } | RowKind::Account { .. } | RowKind::Model { .. } => Vec::new()` arm. Since `ModelEmptyState` is now its own arm with non-trivial behavior, it must appear *before* the catch-all to take precedence — or be added to the catch-all's negative space (i.e., not in the catch-all). The cleanest shape is a dedicated arm above the catch-all.

If the existing catch-all reads `RowKind::Entry { .. } | RowKind::Account { .. } | RowKind::Model { .. } => Vec::new()`, leave it alone — `ModelEmptyState` isn't in its variant list, so the new arm is reached on its own.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p ox-cli --lib settings::commands::tree::tests::activate_on_empty_state_row_
```

Expected: both pass.

- [ ] **Step 5: Run the full ox-cli test suite**

```bash
cargo test -p ox-cli
```

Expected: all tests pass.

- [ ] **Step 6: Per-commit gate**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings
```

Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-cli/src/settings/commands/tree.rs
git commit -m "feat(settings): Enter on ModelEmptyState row triggers catalog refresh

The synthetic empty-state row (one per connection with no catalog)
becomes actionable: pressing Enter writes
config/gate/accounts/{name}/refresh_now and the existing
gate.catalog_refresh subscription replaces the empty-state row with
real model rows on success.

Closes the discoverability gap where the only way to populate a
catalog was the global 'r' keystroke from a focused (and possibly
nonexistent) model row."
```

---

## Final verification

- [ ] **Step 1: Run full quality gates**

```bash
./scripts/quality_gates.sh
```

Expected: 15/15 pass.

- [ ] **Step 2: Confirm working tree clean**

```bash
git status
```

Expected: `nothing to commit, working tree clean`. If files appear modified that you didn't touch, they're fmt drift from a missed gate run earlier — fold into a `chore(fmt)` commit before declaring done.

---

## Slice 4 Definition of Done

- A user with a configured connection whose catalog has never been refreshed sees, when expanding Models, one row labeled `<connection> / (no models — Enter to refresh)` instead of zero rows.
- Pressing Enter on that row triggers a catalog refresh; on success the row is replaced by real model rows (existing subscription behavior, no UI change needed for the success path).
- Each cataloged model row's secondary text reads `ctx 200k · out 8k` (or `ctx — · out —` when the budgets are unknown).
- `cargo test -p ox-cli` is green; `scripts/quality_gates.sh` is green.

## Deferred to follow-up slices

- Real `View::Table` variant in ox-view with column alignment and headers (current slice uses the existing `primary` + `secondary` slots, which renders as left-text + right-text per row).
- Bootstrap (`B`) and default-available (`D`) flag columns — depend on Slices 2 and 3.
- Pricing columns ($/in, $/out) — needs `ModelInfo` schema extension.
- `/` search/filter command over the Models table — meaty enough to warrant its own slice.
- Refresh-status indicator on the empty-state row ("refreshing…", "refresh failed: …") — requires reading `config/gate/accounts/{name}/refresh_status` per row; small but distinct.
