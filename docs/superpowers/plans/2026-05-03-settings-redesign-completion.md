# Settings Redesign — Completion Plan (Slices 2, 3, 5, 6)

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the four remaining slices of the Connections-redesign roadmap so the Settings screen reaches the shape committed to in `docs/superpowers/plans/2026-05-03-settings-connections-roadmap.md`. Slice 1 already shipped (kill `PROTOCOL_OPTIONS`). Slice 4's standalone plan is at `docs/superpowers/plans/2026-05-03-settings-slice-4-models-empty-state.md`. This document covers Slices 2, 3, 5, 6 in execution order, with Slice 4 inserted by reference at its dependency point.

**Architecture:** Same path-MVU primitive throughout. Schema additions are surgical: rename `gate/completions/primary` → `gate/bootstrap` (Slice 2), add `gate/default_available: Vec<ModelKey>` (Slice 3), add `ModelInfoSource::UserEntered` variant (Slice 6). Only Slice 3 touches the kernel; everything else stays inside `ox-cli/src/settings`. Rename of "Account" → "Connection" stays at the UI-string level — broker paths and Rust identifiers keep `account` so the data layer is undisturbed.

**Tech Stack:** Rust, ox-cli settings module, ox-broker subscriptions, ox-kernel (Slice 3 only), ox-gate types.

**Predecessor:** `docs/superpowers/plans/2026-05-03-settings-connections-roadmap.md` (the roadmap), `docs/superpowers/plans/2026-05-03-settings-slice-4-models-empty-state.md` (Slice 4 detail).

---

## 0. Prelude

### Decisions baked in

These were the three open questions from the roadmap. Default-applied here so execution can proceed without further input:

| Q | Decision | Rationale |
|---|---|---|
| Q1 — default-available scope | **Kernel-side gate.** `config/gate/default_available: Vec<ModelKey>`. Kernel reads at thread spawn and gates the tool-callable model set. | The phrasing "default available for a new thread without modification" implies the thread CAN modify access; the default subset is the enforced floor. UI-only filter would be a lie. |
| Q2 — manual entry required fields | **`id`, `max_context_size`, `max_output_tokens`.** Display name auto-fills from `id`. Pricing deferred to a future slice. | Anything less and the kernel can't budget context or build requests. Pricing is presentation, not functionality. |
| Q3 — bootstrap on failing connection | **Allow with inline warning.** Bootstrap toggle never blocks; if `test_status == Failed { .. }`, render an inline warning "(connection failing — bootstrap will retry on next launch)". | User is in control; transient failures shouldn't lock out their own bootstrap choice. |

### Execution order

Dependencies and risk shape the order:

1. **Slice 2 (bootstrap rename + per-row toggle)** — small, low-risk, conceptual cleanup that subsequent slices reference.
2. **Slice 3 (default_available + kernel gate + per-row toggle)** — biggest blast radius (only kernel touch); land early so any kernel surprises surface before the rest piles on top.
3. **Slice 4 (Models empty-state + secondary metadata)** — already planned; execute per its own file. Pointer in §3 below.
4. **Slice 6 (manual model entry)** — extends Slice 4's empty-state row into a "+ add row" affordance; needs Slice 4's `ModelEmptyState` row kind in place first.
5. **Slice 5 (Connection terminology + share-set indicator + joint form)** — pure UI polish + share-set rendering. Lands last so the rename sweeps through all prior slices' user-facing strings in one pass.

### Per-commit gate

Same as Slice 1: `cargo fmt --all -- --check && cargo clippy -p <crate> --all-targets -- -D warnings && cargo test -p <crate>`. Full `scripts/quality_gates.sh` once at the end of each slice.

### Conventions

- Commits one-per-task, message style matches `git log` (`feat(settings): …`, `fix(settings): …`, `refactor(settings): …`).
- Identifier convention: code keeps `account` everywhere (matches `config/gate/accounts/{name}`); user-facing strings use "Connection" after Slice 5. Until Slice 5 lands, leave new strings as "Connection" — that way Slice 5 is just rename-renderer-strings, not rename-everything.
- New schema records are typed via `structfs_serde_store::to_value` / `read_typed`, never raw `Value::Map`.
- Comments explain WHY (per repo convention `feedback_no_phase_or_pr_comments`); no "Slice N" or "ticket #" annotations.

---

## 1. Slice 2 — Bootstrap rename + per-row toggle

**Outcome:** The path `config/gate/completions/primary: CompletionRole` becomes `config/gate/bootstrap: CompletionRole` (same shape, clearer name). Reads cascade: new path first, fall back to legacy. Writes go to both during the migration window so a downgrade doesn't corrupt state. The Models index entry's badge re-points to the new path; the `models.set_primary` command becomes `models.set_bootstrap` (same `P` keystroke, same row behavior). Models rows render a `B` glyph in their badge slot when they're the bootstrap model.

### File Structure

| File | Change |
|---|---|
| `crates/ox-cli/src/settings/bootstrap.rs` | `populate_index_entries`: rename Models entry's badge from `BadgeSource::PrimaryReference` to a new `BadgeSource::BootstrapReference` that resolves from `config/gate/bootstrap`. |
| `crates/ox-types/src/settings.rs` | Add `BadgeSource::BootstrapReference` variant; keep `PrimaryReference` for one release as a deprecated alias for backwards-compat with stored entries. |
| `crates/ox-cli/src/settings/visible_rows.rs` | `resolve_badge` arm for `BootstrapReference` reads `config/gate/bootstrap` (with fallback to `config/gate/completions/primary`). |
| `crates/ox-cli/src/settings/commands/account_model.rs` | `models_set_primary` becomes `models_set_bootstrap`: writes to `config/gate/bootstrap` AND `config/gate/completions/primary` for migration; rename the `ModelsSetPrimary` struct to `ModelsSetBootstrap`. Update binding registration. |
| `crates/ox-cli/src/settings/visible_rows.rs` | Model row's `badge` set to `Some("B".into())` when its (account, model_id) matches the bootstrap CompletionRole. |
| `crates/ox-cli/src/settings/bindings.rs` | Update binding for `P` to point at `models.set_bootstrap` (id rename). |
| Kernel call sites that read primary | Read `config/gate/bootstrap` first; fall back to `config/gate/completions/primary`. Search `grep -rn "completions.*primary" crates/ox-kernel`. |

### Conventions specific to this slice

- Migration shape: read new → fallback to legacy; writes go to both. After one release the legacy can be retired in a follow-up.
- The `B` badge is a single-character text glyph in the existing `ListItem.badge` slot. No new view variant.
- Q3 decision: bootstrap toggle never blocks. If the connection's `test_status` is `Failed`, render the warning inline in the row's `secondary` (after Slice 4 lands secondary). Until Slice 4, the warning lives in the `badge` next to "B" as "B!" with a tooltip-y comment in the diff. Cleaner: defer the warning UI to Slice 4's metadata land — write the bootstrap regardless, no warning in Slice 2 itself.

### Task 2.1: Add `BadgeSource::BootstrapReference` typed variant

**Files:**
- Modify: `crates/ox-types/src/settings.rs`

- [ ] **Step 1: Locate the BadgeSource enum**

```bash
grep -n "enum BadgeSource" crates/ox-types/src/settings.rs
```

- [ ] **Step 2: Add the new variant**

In `crates/ox-types/src/settings.rs`, add `BootstrapReference` to the enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BadgeSource {
    None,
    Static(String),
    SubtreeCount(Path),
    /// Resolves to "{account} / {model}" from `config/gate/completions/primary`.
    /// Deprecated: prefer `BootstrapReference`. Retained for one release so
    /// stored SettingsIndexEntry records written under the old name still
    /// deserialize cleanly.
    PrimaryReference,
    /// Resolves to "{account} / {model}" from `config/gate/bootstrap`,
    /// falling back to `config/gate/completions/primary` for migration.
    BootstrapReference,
}
```

- [ ] **Step 3: Verify the workspace still builds**

```bash
cargo check --workspace
```

Expected: clean. Any `match BadgeSource { ... }` site that's exhaustive gets a missing-arm error — add the missing `BootstrapReference => ...` arm with the same body as `PrimaryReference` for now (Task 2.2 will give it real behavior).

- [ ] **Step 4: Run tests**

```bash
cargo test -p ox-types
```

Expected: pass.

- [ ] **Step 5: Per-commit gate**

```bash
cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/ox-types/src/settings.rs
# Plus any exhaustiveness-arm additions in dependent crates:
git add -u
git commit -m "feat(types): add BadgeSource::BootstrapReference variant

Mirrors PrimaryReference's shape but resolves from the new
config/gate/bootstrap path. PrimaryReference stays for one release
as a deprecated alias so stored SettingsIndexEntry records survive."
```

### Task 2.2: Implement BootstrapReference resolution with legacy fallback

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Write the failing test**

In `visible_rows.rs` test module:

```rust
    #[test]
    fn resolve_badge_bootstrap_reference_reads_new_path() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "bootstrap"),
            to_value(&CompletionRole {
                account: "alpha".into(),
                model_id: "claude-sonnet-4".into(),
            })
            .unwrap(),
        );
        let badge = resolve_badge(&mut snap, &BadgeSource::BootstrapReference);
        assert_eq!(badge.as_deref(), Some("alpha / claude-sonnet-4"));
    }

    #[test]
    fn resolve_badge_bootstrap_reference_falls_back_to_legacy_primary() {
        // Stored config from before the rename only has the legacy path.
        // The badge must still render so the user sees their bootstrap
        // choice on the Models row.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "completions", "primary"),
            to_value(&CompletionRole {
                account: "legacy".into(),
                model_id: "claude-3".into(),
            })
            .unwrap(),
        );
        let badge = resolve_badge(&mut snap, &BadgeSource::BootstrapReference);
        assert_eq!(badge.as_deref(), Some("legacy / claude-3"));
    }

    #[test]
    fn resolve_badge_bootstrap_reference_prefers_new_path_when_both_present() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "completions", "primary"),
            to_value(&CompletionRole {
                account: "legacy".into(),
                model_id: "old-model".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "bootstrap"),
            to_value(&CompletionRole {
                account: "current".into(),
                model_id: "new-model".into(),
            })
            .unwrap(),
        );
        let badge = resolve_badge(&mut snap, &BadgeSource::BootstrapReference);
        assert_eq!(badge.as_deref(), Some("current / new-model"));
    }
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::resolve_badge_bootstrap_
```

Expected: 3 fail. The current `resolve_badge` arm for `BootstrapReference` (added defensively in Task 2.1) returns the same as `PrimaryReference`, which only reads the legacy path — so the "prefers new" and "reads new" tests fail.

- [ ] **Step 3: Implement the new arm**

In `visible_rows.rs`, locate `resolve_badge`. Replace the `BootstrapReference` arm with:

```rust
        BadgeSource::BootstrapReference => read_typed::<CompletionRole>(data, &oxpath!("config", "gate", "bootstrap"))
            .or_else(|| {
                read_typed::<CompletionRole>(
                    data,
                    &oxpath!("config", "gate", "completions", "primary"),
                )
            })
            .map(|role| format!("{} / {}", role.account, role.model_id)),
```

- [ ] **Step 4: Run tests to verify pass**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::resolve_badge_bootstrap_
```

Expected: 3 pass.

- [ ] **Step 5: Per-commit gate + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add crates/ox-cli/src/settings/visible_rows.rs
git commit -m "feat(settings): BootstrapReference badge resolves new path with legacy fallback

resolve_badge for BadgeSource::BootstrapReference reads
config/gate/bootstrap first, falls back to config/gate/completions/primary
when absent. Lets stored configs from before the rename render their
bootstrap badge unchanged."
```

### Task 2.3: Repoint the Models index entry to BootstrapReference

**Files:**
- Modify: `crates/ox-cli/src/settings/bootstrap.rs`

- [ ] **Step 1: Update `populate_index_entries`**

In `crates/ox-cli/src/settings/bootstrap.rs`, find the `models_entry` definition:

```rust
    let models_entry = SettingsIndexEntry {
        id: "models".to_string(),
        label: "Models".to_string(),
        description: "Browse model catalogs and select primary.".to_string(),
        target_cursor: oxpath!("settings", "models"),
        badge: BadgeSource::PrimaryReference,
    };
```

Change two fields:

```rust
    let models_entry = SettingsIndexEntry {
        id: "models".to_string(),
        label: "Models".to_string(),
        description: "Browse model catalogs and tag the bootstrap model.".to_string(),
        target_cursor: oxpath!("settings", "models"),
        badge: BadgeSource::BootstrapReference,
    };
```

- [ ] **Step 2: Update the existing test that asserts the badge variant**

The test `populate_writes_both_entries` in `bootstrap.rs` currently asserts:

```rust
        match models.badge {
            ...
            other => panic!("unexpected: {:?}", other),
        }
```

If it pattern-matches on `PrimaryReference` directly, change the matched variant to `BootstrapReference`. Search the test:

```bash
grep -n "PrimaryReference" crates/ox-cli/src/settings/bootstrap.rs
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p ox-cli --lib settings::bootstrap
```

Expected: pass.

- [ ] **Step 4: Per-commit gate + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add crates/ox-cli/src/settings/bootstrap.rs
git commit -m "feat(settings): Models index entry uses BootstrapReference badge

The badge text is unchanged for users with existing config (via the
legacy fallback in resolve_badge), but the description string and the
source-of-truth path are now bootstrap-named."
```

### Task 2.4: Rename `models.set_primary` → `models.set_bootstrap`

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Write the failing test**

In `account_model.rs` test module, add:

```rust
    #[test]
    fn models_set_bootstrap_writes_both_paths_for_migration() {
        let mut snap = SettingsSnapshot::empty();
        select_model(&mut snap, "alpha", "claude-sonnet-4");
        let writes = run_cmd(&ModelsSetBootstrap::new(), &mut snap);
        // Expect two writes: new path AND legacy path. Order doesn't matter.
        assert_eq!(writes.len(), 2);
        let paths: Vec<String> = writes.iter().map(|w| w.path.to_string()).collect();
        assert!(paths.iter().any(|p| p == "config/gate/bootstrap"));
        assert!(paths.iter().any(|p| p == "config/gate/completions/primary"));
        // Both must encode the same CompletionRole.
        for w in &writes {
            let role: CompletionRole =
                structfs_serde_store::from_value(w.record.as_value().unwrap().clone()).unwrap();
            assert_eq!(role.account, "alpha");
            assert_eq!(role.model_id, "claude-sonnet-4");
        }
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib settings::commands::account_model::tests::models_set_bootstrap_
```

Expected: fail with "cannot find type `ModelsSetBootstrap`".

- [ ] **Step 3: Rename the struct + extend the writes**

In `account_model.rs`, locate the existing `ModelsSetPrimary` command struct (around line 116):

```rust
command! {
    struct_name: ModelsSetPrimary,
    id: "models.set_primary",
    title: "Set as Primary",
    description: "Bind config/gate/completions/primary to the selected (account, model).",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_set_primary(snap),
}
```

Replace with:

```rust
command! {
    struct_name: ModelsSetBootstrap,
    id: "models.set_bootstrap",
    title: "Set as Bootstrap",
    description: "Bind config/gate/bootstrap to the selected (account, model). Also writes the legacy config/gate/completions/primary path during the migration window.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_set_bootstrap(snap),
}
```

Locate the `models_set_primary` function (around line 355). Rename and extend:

```rust
fn models_set_bootstrap(data: &mut dyn Reader) -> Vec<Write> {
    let key = match read_selected_model(data) {
        Some(k) => k,
        None => return Vec::new(),
    };
    let role = CompletionRole {
        account: key.account,
        model_id: key.model_id,
    };
    let value = match to_value(&role) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "models.set_bootstrap: failed to encode CompletionRole");
            return Vec::new();
        }
    };
    // Two writes during the migration window: the new path is the
    // source of truth, the legacy path stays in lockstep so a downgrade
    // (or any kernel call site that hasn't yet switched) sees the same
    // bootstrap choice. The legacy write can be removed in a follow-up
    // once every reader has migrated.
    vec![
        Write {
            path: oxpath!("config", "gate", "bootstrap"),
            record: Record::parsed(value.clone()),
        },
        Write {
            path: oxpath!("config", "gate", "completions", "primary"),
            record: Record::parsed(value),
        },
    ]
}
```

Delete the old `models_set_primary` function entirely.

- [ ] **Step 4: Update binding registration**

In `crates/ox-cli/src/settings/bindings.rs`, search for `models.set_primary`:

```bash
grep -n "models.set_primary\|models\\.set_primary" crates/ox-cli/src/settings/bindings.rs
```

For each occurrence, change to `models.set_bootstrap`. Search the rest of the codebase to make sure no other consumer references the old id:

```bash
grep -rn "models.set_primary\|models\\.set_primary" crates --include="*.rs"
```

Update each match (if any).

- [ ] **Step 5: Update existing tests that reference the old name**

```bash
grep -rn "ModelsSetPrimary\|models_set_primary\|models.set_primary" crates --include="*.rs"
```

Replace each occurrence with the new name. The e2e test `navigate_index_to_models_set_primary` should be renamed to `navigate_index_to_models_set_bootstrap` and its assertion path should expand to verify both writes.

- [ ] **Step 6: Run all tests**

```bash
cargo test -p ox-cli
```

Expected: pass.

- [ ] **Step 7: Per-commit gate + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings
git add -u
git commit -m "refactor(settings): rename models.set_primary → models.set_bootstrap

The 'primary' name conflated 'global default' with 'first-turn model';
in this codebase a thread can call any model at any time (a completion
is just a tool call), so the only thing that's actually configurable
is the bootstrap model used before the thread or user has picked
otherwise. The new name reflects that.

Writes go to both config/gate/bootstrap (new source of truth) and
config/gate/completions/primary (legacy) so a downgrade or a
not-yet-migrated reader still sees the user's choice. Legacy write
retires in a follow-up after one release."
```

### Task 2.5: Migrate kernel readers from primary to bootstrap

**Files:**
- Modify: every kernel-side reader of `config/gate/completions/primary`. Find them with:

```bash
grep -rn "completions.*primary\|completions/primary" crates/ox-kernel crates/ox-cli/src/agents 2>/dev/null
```

Likely sites include `crates/ox-kernel/src/run.rs` and any place that resolves the active completion role for a fresh thread.

- [ ] **Step 1: Identify call sites**

Run the grep. For each result, decide whether it's a read (needs migration) or a write (stays — only the command writes, and we already do both paths).

- [ ] **Step 2: Write the failing test**

Pick the lowest-level kernel reader (likely in `crates/ox-kernel/src/run.rs`). Add a test that seeds `config/gate/bootstrap` only (no legacy primary) and verifies the kernel reads it.

Pattern (adapt to the actual fixture conventions in the kernel test module):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_resolves_bootstrap_role_from_new_path() {
    // Seed only the new path; legacy is absent.
    let (broker, client) = setup_test_broker().await;
    client
        .write_typed(
            &path!("config/gate/bootstrap"),
            &CompletionRole {
                account: "personal".into(),
                model_id: "claude-sonnet-4".into(),
            },
        )
        .await
        .unwrap();
    let resolved = resolve_completion_role(&client).await.unwrap();
    assert_eq!(resolved.account, "personal");
    assert_eq!(resolved.model_id, "claude-sonnet-4");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_falls_back_to_legacy_primary_when_bootstrap_absent() {
    let (broker, client) = setup_test_broker().await;
    client
        .write_typed(
            &path!("config/gate/completions/primary"),
            &CompletionRole {
                account: "legacy".into(),
                model_id: "old-model".into(),
            },
        )
        .await
        .unwrap();
    let resolved = resolve_completion_role(&client).await.unwrap();
    assert_eq!(resolved.account, "legacy");
}
```

The function name `resolve_completion_role` is illustrative — match the actual reader name in the kernel.

- [ ] **Step 3: Implement the migration in the reader**

The reader currently looks like:

```rust
let role: CompletionRole = client
    .read_typed(&path!("config/gate/completions/primary"))
    .await?
    .ok_or(...)?;
```

Change to:

```rust
let role: CompletionRole = match client
    .read_typed(&path!("config/gate/bootstrap"))
    .await?
{
    Some(r) => r,
    None => client
        .read_typed(&path!("config/gate/completions/primary"))
        .await?
        .ok_or(...)?,
};
```

Apply this transformation at every reader identified in Step 1.

- [ ] **Step 4: Run kernel tests**

```bash
cargo test -p ox-kernel
```

Expected: pass.

- [ ] **Step 5: Per-commit gate + commit**

```bash
cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings
git add -u
git commit -m "feat(kernel): read bootstrap role from new path with legacy fallback

Kernel readers of the bootstrap (formerly 'primary') CompletionRole
now check config/gate/bootstrap first and fall back to
config/gate/completions/primary. Lets the new path become the source
of truth for fresh installs while preserving stored state for
existing users."
```

### Task 2.6: Surface the bootstrap badge on Model rows

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn model_row_badge_marks_bootstrap_choice() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["claude-sonnet-4", "claude-opus-4"]);
        snap.insert(
            &oxpath!("config", "gate", "bootstrap"),
            to_value(&CompletionRole {
                account: "alpha".into(),
                model_id: "claude-sonnet-4".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let bootstrap_row = rows
            .iter()
            .find(|r| matches!(
                &r.kind,
                RowKind::Model { account, model_id }
                    if account == "alpha" && model_id == "claude-sonnet-4"
            ))
            .expect("bootstrap row");
        assert_eq!(bootstrap_row.badge.as_deref(), Some("B"));
        let other_row = rows
            .iter()
            .find(|r| matches!(
                &r.kind,
                RowKind::Model { account, model_id }
                    if account == "alpha" && model_id == "claude-opus-4"
            ))
            .expect("non-bootstrap row");
        assert!(other_row.badge.is_none());
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::model_row_badge_marks_bootstrap_choice
```

Expected: fail. Current code sets `badge: None` for every model row.

- [ ] **Step 3: Read the bootstrap once, decorate matching row**

In `visible_rows.rs`, modify `append_model_rows` to read the bootstrap role once (with legacy fallback), then set the badge on matching rows:

```rust
fn append_model_rows(rows: &mut Vec<VisibleRow>, data: &mut dyn Reader, expanded: &[String]) {
    let bootstrap: Option<ox_gate::CompletionRole> =
        read_typed(data, &oxpath!("config", "gate", "bootstrap")).or_else(|| {
            read_typed(data, &oxpath!("config", "gate", "completions", "primary"))
        });

    let account_names = child_names_under(data, "config/gate/accounts");
    for account_name in &account_names {
        // ... existing models_path / read_typed ...
        for m in models {
            let badge = if bootstrap
                .as_ref()
                .is_some_and(|r| r.account == *account_name && r.model_id == m.id)
            {
                Some("B".to_string())
            } else {
                None
            };
            rows.push(VisibleRow {
                // ... existing fields ...
                badge,
                // ...
            });
            // ... existing field-row append ...
        }
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p ox-cli --lib settings::visible_rows
```

Expected: pass.

- [ ] **Step 5: Per-commit gate + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add crates/ox-cli/src/settings/visible_rows.rs
git commit -m "feat(settings): mark bootstrap model row with 'B' badge

The (account, model_id) pair currently bound to config/gate/bootstrap
(or the legacy primary path) renders with badge='B' so the user can
see at a glance which model their fresh threads start on."
```

### Slice 2 Definition of Done

- `config/gate/bootstrap` is the source of truth for the bootstrap CompletionRole; `config/gate/completions/primary` stays in lockstep via the dual-write.
- Kernel readers consult the new path first.
- Models tree shows `B` badge on the bootstrap row.
- The `P` keystroke writes both paths; a fresh install uses the new path; an upgraded install keeps working.
- `cargo test -p ox-cli && cargo test -p ox-kernel` is green.

---

## 2. Slice 3 — `default_available` record + kernel gate + per-row toggle

**Outcome:** A new typed record `config/gate/default_available: Vec<ModelKey>` controls which `(account, model_id)` pairs a freshly-spawned thread sees in its tool-callable model set. The Models tree gets a `D` badge on each row that's in the set; pressing `d` on a focused row toggles membership.

### File Structure

| File | Change |
|---|---|
| `crates/ox-types/src/settings.rs` (or wherever `ModelKey` lives) | No struct change — `ModelKey` already exists. |
| `crates/ox-cli/src/settings/visible_rows.rs` | `append_model_rows` reads `default_available` once; sets badge `D` (or `D B` if also bootstrap — handle the conjunction). |
| `crates/ox-cli/src/settings/commands/account_model.rs` | New command `models.toggle_default`: read current set, toggle the focused row's ModelKey, write back. |
| `crates/ox-cli/src/settings/bindings.rs` | Bind `d` (no modifiers) under the `settings/models` cursor prefix to `models.toggle_default`. NOTE: `d` is currently bound to `accounts.delete_confirm` under `settings/accounts` prefix — unaffected. |
| ox-kernel: thread spawn / tool resolution | Read `default_available` at thread spawn; restrict the tool-callable model set to its members. |

### Conventions specific to this slice

- The badge slot holds at most one short string. With both bootstrap and default-available active, the badge becomes `"D B"` (space-separated). Keep it terse.
- The `models.toggle_default` command writes the entire `Vec<ModelKey>` back each time (read-modify-write); StructFS doesn't have a delta-add primitive on this path. With dozens of rows the cost is negligible.
- Empty `default_available` semantics: when the record is absent, the kernel should treat the intent as "all cataloged models default-available." This preserves backwards compat — existing installs see no behavior change until the user explicitly tags a subset. **First explicit `D` toggle creates the record with the toggled key as its sole member.** Removing the last entry deletes the record (back to "all available").

### Task 3.1: New `models.toggle_default` command

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn toggle_default_adds_to_empty_set() {
        let mut snap = SettingsSnapshot::empty();
        select_model(&mut snap, "alpha", "claude-sonnet-4");
        let writes = run_cmd(&ModelsToggleDefault::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("config", "gate", "default_available"));
        let set: Vec<ModelKey> =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].account, "alpha");
        assert_eq!(set[0].model_id, "claude-sonnet-4");
    }

    #[test]
    fn toggle_default_removes_from_set_when_already_present() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "default_available"),
            to_value(&vec![ModelKey {
                account: "alpha".into(),
                model_id: "claude-sonnet-4".into(),
            }])
            .unwrap(),
        );
        select_model(&mut snap, "alpha", "claude-sonnet-4");
        let writes = run_cmd(&ModelsToggleDefault::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        // Removing the last entry should write Null to delete the record
        // (back to implicit "all cataloged models default-available").
        match &writes[0].record {
            Record::Parsed(Value::Null) => {}
            other => panic!("expected null delete, got {other:?}"),
        }
    }

    #[test]
    fn toggle_default_removes_one_keeps_rest() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "default_available"),
            to_value(&vec![
                ModelKey {
                    account: "alpha".into(),
                    model_id: "m1".into(),
                },
                ModelKey {
                    account: "alpha".into(),
                    model_id: "m2".into(),
                },
            ])
            .unwrap(),
        );
        select_model(&mut snap, "alpha", "m1");
        let writes = run_cmd(&ModelsToggleDefault::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        let set: Vec<ModelKey> =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].model_id, "m2");
    }

    #[test]
    fn toggle_default_no_op_with_no_selected_model() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&ModelsToggleDefault::new(), &mut snap);
        assert!(writes.is_empty());
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib settings::commands::account_model::tests::toggle_default_
```

Expected: 4 fail.

- [ ] **Step 3: Implement the command + helper**

In `account_model.rs`, near the other model commands, add:

```rust
command! {
    struct_name: ModelsToggleDefault,
    id: "models.toggle_default",
    title: "Toggle Default-Available",
    description: "Add or remove the focused (account, model) from the default-available set.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_toggle_default(snap),
}
```

And the function:

```rust
fn models_toggle_default(data: &mut dyn Reader) -> Vec<Write> {
    let key = match read_selected_model(data) {
        Some(k) => k,
        None => return Vec::new(),
    };
    let current: Vec<ModelKey> =
        read_typed(data, &oxpath!("config", "gate", "default_available")).unwrap_or_default();

    let mut next = current.clone();
    if let Some(pos) = next
        .iter()
        .position(|k| k.account == key.account && k.model_id == key.model_id)
    {
        next.remove(pos);
    } else {
        next.push(key);
    }

    // Empty set → delete the record so kernel falls back to "all
    // cataloged models default-available." Any non-empty set writes
    // verbatim.
    if next.is_empty() {
        return vec![Write {
            path: oxpath!("config", "gate", "default_available"),
            record: Record::parsed(Value::Null),
        }];
    }

    let value = match to_value(&next) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "models.toggle_default: failed to encode");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("config", "gate", "default_available"),
        record: Record::parsed(value),
    }]
}
```

Register `ModelsToggleDefault` in the `register_all` list.

- [ ] **Step 4: Run tests**

```bash
cargo test -p ox-cli --lib settings::commands::account_model::tests::toggle_default_
```

Expected: pass.

- [ ] **Step 5: Per-commit gate + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "feat(settings): add models.toggle_default command

Reads config/gate/default_available, toggles the focused model's
ModelKey, writes back. Empty result deletes the record (kernel
falls back to 'all cataloged models default-available')."
```

### Task 3.2: Bind `d` to `models.toggle_default` under settings/models

**Files:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Find the models-prefix binding section**

```bash
grep -n "settings/models\|models_subtree\|settings\", \"models\"" crates/ox-cli/src/settings/bindings.rs
```

- [ ] **Step 2: Add the binding**

Locate the `models_subtree` block (around the `models.set_primary` / `models.set_bootstrap` registration). Add:

```rust
    bind_prefix(
        reg,
        models_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('d'),
        "models.toggle_default",
    );
```

Place it after the existing `account.refresh` (`r`) binding under the same prefix.

- [ ] **Step 3: Add test verifying resolution**

```rust
    #[test]
    fn models_d_resolves_to_toggle_default() {
        let mut reg = BindingRegistry::new();
        register(&mut reg);
        let cmd_id = reg
            .lookup(
                &oxpath!("settings", "models"),
                &key(no_mods(), KeyCodeRepr::Char('d')),
            )
            .expect("d under settings/models resolves");
        assert_eq!(cmd_id.0, "models.toggle_default");
    }
```

- [ ] **Step 4: Verify + commit**

```bash
cargo test -p ox-cli --lib settings::bindings::tests::models_d_resolves_to_toggle_default
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings
git add crates/ox-cli/src/settings/bindings.rs
git commit -m "feat(settings): bind d under settings/models to models.toggle_default

The 'd' key is already bound to accounts.delete_confirm under
settings/accounts; binding scopes are disjoint so the same key
serves two roles in two cursor regions without ambiguity."
```

### Task 3.3: Render `D` badge on default-available rows

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn model_row_badge_marks_default_available_pair() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1", "m2"]);
        snap.insert(
            &oxpath!("config", "gate", "default_available"),
            to_value(&vec![ModelKey {
                account: "alpha".into(),
                model_id: "m1".into(),
            }])
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let m1_row = rows
            .iter()
            .find(|r| matches!(
                &r.kind,
                RowKind::Model { account, model_id }
                    if account == "alpha" && model_id == "m1"
            ))
            .expect("m1 row");
        assert_eq!(m1_row.badge.as_deref(), Some("D"));
        let m2_row = rows
            .iter()
            .find(|r| matches!(
                &r.kind,
                RowKind::Model { account, model_id }
                    if account == "alpha" && model_id == "m2"
            ))
            .expect("m2 row");
        assert!(m2_row.badge.is_none());
    }

    #[test]
    fn model_row_badge_combines_default_and_bootstrap() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        snap.insert(
            &oxpath!("config", "gate", "default_available"),
            to_value(&vec![ModelKey {
                account: "alpha".into(),
                model_id: "m1".into(),
            }])
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "bootstrap"),
            to_value(&CompletionRole {
                account: "alpha".into(),
                model_id: "m1".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let row = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Model { .. }))
            .expect("model row");
        // Order: D first, then B (D is the multi-select common case).
        assert_eq!(row.badge.as_deref(), Some("D B"));
    }
```

- [ ] **Step 2: Verify failure**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::model_row_badge_marks_default_available_pair
cargo test -p ox-cli --lib settings::visible_rows::tests::model_row_badge_combines_default_and_bootstrap
```

Expected: 2 fail.

- [ ] **Step 3: Read default_available + combine with bootstrap badge**

In `visible_rows.rs::append_model_rows`, near where `bootstrap` is read (Slice 2 Task 2.6), also read the default-available set:

```rust
    let bootstrap: Option<ox_gate::CompletionRole> =
        read_typed(data, &oxpath!("config", "gate", "bootstrap")).or_else(|| {
            read_typed(data, &oxpath!("config", "gate", "completions", "primary"))
        });
    let default_set: Vec<ModelKey> =
        read_typed(data, &oxpath!("config", "gate", "default_available")).unwrap_or_default();
```

Replace the per-model badge computation with a combined version:

```rust
            let is_bootstrap = bootstrap
                .as_ref()
                .is_some_and(|r| r.account == *account_name && r.model_id == m.id);
            let is_default = default_set
                .iter()
                .any(|k| k.account == *account_name && k.model_id == m.id);
            let badge = match (is_default, is_bootstrap) {
                (true, true) => Some("D B".to_string()),
                (true, false) => Some("D".to_string()),
                (false, true) => Some("B".to_string()),
                (false, false) => None,
            };
```

- [ ] **Step 4: Verify + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add crates/ox-cli/src/settings/visible_rows.rs
git commit -m "feat(settings): render D badge on default-available model rows

Each model row's badge combines D (in default-available set) and B
(bootstrap) into a space-separated string. Empty default-available
record is treated as 'all cataloged models default-available' so
existing configs without an explicit subset show only B (or nothing)."
```

### Task 3.4: Kernel gate — read default_available at thread spawn

**Files:**
- Modify: ox-kernel files responsible for assembling the tool-callable model set per thread. Find with:

```bash
grep -rn "ModelKey\|default_available\|callable.*model\|tool.*model" crates/ox-kernel --include="*.rs"
```

The hook lives wherever a fresh thread's model surface is determined. Look for the function that builds the per-thread "what completions can this thread issue" structure.

- [ ] **Step 1: Identify the gate point**

Read the relevant kernel files. Find the function (likely in `crates/ox-kernel/src/run.rs` or similar) that decides which models are tool-callable. The semantics are:

- Read `config/gate/default_available: Vec<ModelKey>`.
- If the record is absent or empty, the gate is open (any cataloged model).
- If present and non-empty, restrict the tool-callable set to its members.

- [ ] **Step 2: Write the failing test**

In the kernel test module:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_spawn_honors_default_available_subset() {
    let (broker, client) = setup_test_broker().await;
    // Three cataloged models on one connection.
    seed_account_with_models(&client, "alpha", &["m1", "m2", "m3"]).await;
    // Default-available restricts to one.
    client
        .write_typed(
            &path!("config/gate/default_available"),
            &vec![ModelKey {
                account: "alpha".into(),
                model_id: "m2".into(),
            }],
        )
        .await
        .unwrap();
    let callable = compute_callable_models_for_new_thread(&client).await.unwrap();
    assert_eq!(callable.len(), 1);
    assert_eq!(callable[0].account, "alpha");
    assert_eq!(callable[0].model_id, "m2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_default_available_means_all_cataloged_models_callable() {
    let (broker, client) = setup_test_broker().await;
    seed_account_with_models(&client, "alpha", &["m1", "m2"]).await;
    // No default_available record.
    let callable = compute_callable_models_for_new_thread(&client).await.unwrap();
    assert_eq!(callable.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_default_available_means_all_cataloged_models_callable() {
    // Defensive: a stored empty Vec should behave the same as absent.
    let (broker, client) = setup_test_broker().await;
    seed_account_with_models(&client, "alpha", &["m1", "m2"]).await;
    client
        .write_typed::<Vec<ModelKey>>(&path!("config/gate/default_available"), &vec![])
        .await
        .unwrap();
    let callable = compute_callable_models_for_new_thread(&client).await.unwrap();
    assert_eq!(callable.len(), 2);
}
```

`compute_callable_models_for_new_thread` and `seed_account_with_models` are illustrative names — match the actual kernel API.

- [ ] **Step 3: Verify failure**

```bash
cargo test -p ox-kernel
```

Expected: 3 fail.

- [ ] **Step 4: Implement the gate**

In the kernel reader function, add the default_available check:

```rust
async fn compute_callable_models_for_new_thread(
    client: &ClientHandle,
) -> Result<Vec<ModelKey>, Error> {
    let cataloged = enumerate_all_cataloged_models(client).await?;
    let default_available: Option<Vec<ModelKey>> = client
        .read_typed(&path!("config/gate/default_available"))
        .await?;
    match default_available {
        Some(set) if !set.is_empty() => Ok(cataloged
            .into_iter()
            .filter(|key| {
                set.iter().any(|allow| {
                    allow.account == key.account && allow.model_id == key.model_id
                })
            })
            .collect()),
        _ => Ok(cataloged),
    }
}
```

`enumerate_all_cataloged_models` already exists (or its equivalent) — adapt to whatever the actual kernel surface looks like.

- [ ] **Step 5: Verify + commit**

```bash
cargo test -p ox-kernel
cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings
git add -u
git commit -m "feat(kernel): gate per-thread callable models by config/gate/default_available

When the default_available record is present and non-empty, the
kernel restricts a fresh thread's tool-callable model set to its
members. Absent or empty record means 'all cataloged models
callable' — preserves backwards-compat with installs that haven't
explicitly tagged a subset."
```

### Slice 3 Definition of Done

- `config/gate/default_available: Vec<ModelKey>` exists as a typed record; absent/empty means "all cataloged."
- Pressing `d` on a focused Models row toggles membership.
- Models tree shows `D` badge on rows in the set; combined with `B` if also bootstrap.
- ox-kernel restricts a fresh thread's callable model set to the explicit subset when present.
- All workspace tests green.

---

## 3. Slice 4 — Models empty-state + secondary metadata

**This slice is fully specified in `docs/superpowers/plans/2026-05-03-settings-slice-4-models-empty-state.md`.** Execute it as written. Four tasks, ~20 minutes of focused work.

After Slice 4 lands, return to this document for Slice 6.

---

## 4. Slice 6 — Manual model entry (`+ add row`)

**Outcome:** A connection that can't auto-enumerate (no `/models` endpoint, refresh failed, or just an unsupported provider) gets an inline "+ add row" affordance under its empty-state row. The user inputs `id`, `max_context_size`, `max_output_tokens`; the entry lands in `config/gate/accounts/{name}/models` with `source: ModelInfoSource::UserEntered`.

### File Structure

| File | Change |
|---|---|
| `crates/ox-gate/src/lib.rs` (or `crates/ox-types`, wherever `ModelInfoSource` lives) | Add `ModelInfoSource::UserEntered` variant. |
| `crates/ox-cli/src/settings/visible_rows.rs` | When the empty-state row is present, also emit a `RowKind::ModelAddManual { account }` row directly under it labeled "+ add model manually". |
| `crates/ox-cli/src/settings/commands/tree.rs` | `tree.activate` on `ModelAddManual` opens an inline three-field form (id → max_context_size → max_output_tokens). |
| `crates/ox-cli/src/settings/commands/edit.rs` | New edit-mode flow for the three fields; commit writes a new ModelInfo to the connection's catalog. |

### Conventions specific to this slice

- Inline form lives in the existing `ui/settings/edit_*` paths but with a new `edit_kind: "manual_model"` discriminator. Three sequential field commits build up the buffered ModelInfo; final Enter commits it.
- Validation:
  - `id`: non-empty after trim. Reject empty.
  - `max_context_size`: parse as u32, reject zero or non-numeric.
  - `max_output_tokens`: parse as u32, reject zero or non-numeric.
- New entry always has `source: ModelInfoSource::UserEntered`. Display name defaults to id.
- The "+ add row" appears only when the empty-state row is present. In a future slice it could also appear after the last model row of any connection (lets users add custom entries even when the catalog isn't empty).

### Task 6.1: Add `ModelInfoSource::UserEntered` variant

**Files:**
- Modify: `crates/ox-gate/src/lib.rs` (or wherever `ModelInfoSource` is defined — search `grep -rn "enum ModelInfoSource" crates`).

- [ ] **Step 1: Add the variant**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelInfoSource {
    Server,
    KnownTable,
    UserEntered,
}
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

Add `UserEntered => ...` arms to any exhaustive match the compiler complains about. Default behavior: treat the same as `Server` for any non-display logic.

- [ ] **Step 3: Commit**

```bash
git add -u
git commit -m "feat(types): add ModelInfoSource::UserEntered variant

For models the user adds by hand because the connection can't
enumerate them automatically (no /models endpoint, refresh failed,
unsupported provider). Treated the same as Server everywhere except
provenance display."
```

### Task 6.2: Add `RowKind::ModelAddManual` and emit it under empty-state rows

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn empty_state_is_followed_by_add_manual_row() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account(&mut snap, "alpha"); // no models
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let empty_idx = rows
            .iter()
            .position(|r| matches!(&r.kind, RowKind::ModelEmptyState { .. }))
            .expect("empty-state row");
        let add_idx = rows
            .iter()
            .position(|r| matches!(&r.kind, RowKind::ModelAddManual { .. }))
            .expect("add-manual row");
        assert_eq!(add_idx, empty_idx + 1);
        if let RowKind::ModelAddManual { account } = &rows[add_idx].kind {
            assert_eq!(account, "alpha");
        }
        assert!(rows[add_idx].label.contains("+ add model manually"));
    }
```

- [ ] **Step 2: Verify failure**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::empty_state_is_followed_by_add_manual_row
```

- [ ] **Step 3: Add the variant + emit the row**

In `RowKind`:

```rust
    /// Inline "+ add model manually" row that appears directly under a
    /// ModelEmptyState row. Activating it opens the three-field manual
    /// entry form. Future iteration may also emit this after the last
    /// model row of a non-empty catalog so users can add custom entries
    /// alongside auto-enumerated ones.
    ModelAddManual { account: String },
```

In `append_model_rows`, after pushing the `ModelEmptyState` row, push the `ModelAddManual` row at the same depth:

```rust
        if models.is_empty() {
            // ... existing empty-state push ...
            let add_path = row_path(&[
                "settings",
                "models",
                &safe_component(account_name),
                "_add",
            ]);
            rows.push(VisibleRow {
                path: add_path,
                depth: 1,
                label: format!("{} / + add model manually", account_name),
                secondary: None,
                badge: None,
                kind: RowKind::ModelAddManual {
                    account: account_name.clone(),
                },
                expandable: false,
                expanded: false,
            });
            continue;
        }
```

- [ ] **Step 4: Add `ModelAddManual` arm to any exhaustive match**

```bash
cargo build -p ox-cli
```

Add `RowKind::ModelAddManual { .. } => Vec::new()` arms wherever the compiler flags missing patterns. Real behavior comes in Task 6.3.

- [ ] **Step 5: Verify + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add -u
git commit -m "feat(settings): emit ModelAddManual row under each empty connection

Synthetic '+ add model manually' row appears directly under each
ModelEmptyState row at depth 1. Activation behavior comes in the
next commit; this commit reserves the row kind and the path."
```

### Task 6.3: Wire activate on `ModelAddManual` to open the inline form

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/tree.rs`
- Modify: `crates/ox-cli/src/settings/commands/edit.rs`

- [ ] **Step 1: Define the inline-form state shape**

In `crates/ox-cli/src/settings/commands/edit.rs`, document the three-field state lives at:

- `ui/settings/manual_model/account: String` — which connection we're adding to
- `ui/settings/manual_model/stage: "id" | "ctx" | "out"` — which field is currently being edited
- `ui/settings/manual_model/buffer: String` — the live buffer
- `ui/settings/manual_model/staged_id: String` — committed id from previous stage
- `ui/settings/manual_model/staged_ctx: String` — committed ctx (raw text) from previous stage

Adding doc comments inline at the relevant write sites is sufficient — no separate types file needed.

- [ ] **Step 2: Write the failing test for activate**

In `tree.rs` test module:

```rust
    #[test]
    fn activate_on_add_manual_row_initializes_form_at_id_stage() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        set_focused(&mut snap, "settings/models/alpha/_add");

        let writes = run(&TreeActivate::new(), &mut snap);
        // Expect: account, stage="id", buffer="", edit_mode=true → 4 writes.
        assert_eq!(writes.len(), 4);
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.clone()))
            .collect();
        assert!(by_path.contains_key("ui/settings/manual_model/account"));
        assert!(by_path.contains_key("ui/settings/manual_model/stage"));
        assert!(by_path.contains_key("ui/settings/manual_model/buffer"));
        assert!(by_path.contains_key("ui/settings/edit_mode"));
    }
```

- [ ] **Step 3: Implement activate arm**

In `tree.rs::activate`, add the arm before the catch-all:

```rust
            RowKind::ModelAddManual { account } => begin_manual_model(data, account),
```

In `edit.rs`, add:

```rust
pub(crate) fn begin_manual_model(
    _data: &mut dyn Reader,
    account: &str,
) -> Vec<ox_types::subscription::Write> {
    use ox_types::subscription::Write;
    use structfs_core_store::Record;
    use structfs_serde_store::to_value;

    let account_value = to_value(&account.to_string()).expect("string serializes");
    let stage_value = to_value(&"id".to_string()).expect("string serializes");
    vec![
        Write {
            path: ox_path::oxpath!("ui", "settings", "manual_model", "account"),
            record: Record::parsed(account_value),
        },
        Write {
            path: ox_path::oxpath!("ui", "settings", "manual_model", "stage"),
            record: Record::parsed(stage_value),
        },
        Write {
            path: ox_path::oxpath!("ui", "settings", "manual_model", "buffer"),
            record: Record::parsed(structfs_core_store::Value::String(String::new())),
        },
        Write {
            path: ox_path::oxpath!("ui", "settings", "edit_mode"),
            record: Record::parsed(structfs_core_store::Value::Bool(true)),
        },
    ]
}
```

- [ ] **Step 4: Verify + commit**

```bash
cargo test -p ox-cli --lib settings::commands::tree::tests::activate_on_add_manual_row_initializes_form_at_id_stage
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings
git add crates/ox-cli/src/settings/commands/tree.rs crates/ox-cli/src/settings/commands/edit.rs
git commit -m "feat(settings): activate on ModelAddManual seeds the inline form

Pressing Enter on the '+ add model manually' row initializes the
three-stage form state (account, stage='id', empty buffer) and
flips edit_mode on. The dispatcher's edit-mode branch then routes
keystrokes into the buffer; commit (next commit) advances stages
and ultimately writes the new ModelInfo."
```

### Task 6.4: Stage advancement and final commit

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/edit.rs`

- [ ] **Step 1: Write the failing tests**

In `edit.rs` test module:

```rust
    #[test]
    fn manual_model_commit_id_advances_to_ctx_stage() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "account"),
            Value::String("alpha".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("id".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("custom-model".into()),
        );
        let writes = run(&Commit::new(), &mut snap);
        // Expect stage="ctx", staged_id="custom-model", buffer="" → 3 writes.
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.as_value().unwrap().clone()))
            .collect();
        assert_eq!(
            by_path.get("ui/settings/manual_model/stage").unwrap(),
            &Value::String("ctx".into())
        );
        assert_eq!(
            by_path.get("ui/settings/manual_model/staged_id").unwrap(),
            &Value::String("custom-model".into())
        );
        assert_eq!(
            by_path.get("ui/settings/manual_model/buffer").unwrap(),
            &Value::String(String::new())
        );
    }

    #[test]
    fn manual_model_commit_id_rejects_empty() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("id".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("   ".into()),
        );
        let writes = run(&Commit::new(), &mut snap);
        assert!(writes.is_empty(), "empty/whitespace id must not advance");
    }

    #[test]
    fn manual_model_commit_ctx_rejects_non_numeric() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("ctx".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("not-a-number".into()),
        );
        let writes = run(&Commit::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn manual_model_commit_out_writes_full_modelinfo_and_clears_form() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "account"),
            Value::String("alpha".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("out".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "staged_id"),
            Value::String("custom-model".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "staged_ctx"),
            Value::String("100000".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("8000".into()),
        );
        let writes = run(&Commit::new(), &mut snap);
        // The catalog write goes to config/gate/accounts/alpha/models;
        // form clears via several deletes; edit_mode flips off.
        let catalog_write = writes
            .iter()
            .find(|w| {
                w.path.to_string() == "config/gate/accounts/alpha/models"
            })
            .expect("catalog write");
        let models: Vec<ox_gate::ModelInfo> = structfs_serde_store::from_value(
            catalog_write.record.as_value().unwrap().clone(),
        )
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "custom-model");
        assert_eq!(models[0].max_context_size, Some(100_000));
        assert_eq!(models[0].max_output_tokens, Some(8_000));
        assert!(matches!(models[0].source, ox_gate::ModelInfoSource::UserEntered));
    }
```

- [ ] **Step 2: Verify failure**

```bash
cargo test -p ox-cli --lib settings::commands::edit::tests::manual_model_
```

Expected: 4 fail.

- [ ] **Step 3: Extend the existing `Commit` command**

In `edit.rs`, locate the `Commit` command's run body. Add a manual-model branch *before* the existing field-commit logic:

```rust
fn commit(data: &mut dyn Reader) -> Vec<Write> {
    // Manual-model form takes precedence: when a manual_model/stage
    // value is set, route Enter through the staged form's state machine
    // rather than the regular field-commit path.
    if let Some(stage) = read_typed::<String>(data, &oxpath!("ui", "settings", "manual_model", "stage")) {
        return commit_manual_model(data, &stage);
    }
    // ... existing commit body ...
}

fn commit_manual_model(data: &mut dyn Reader, stage: &str) -> Vec<Write> {
    let buffer: String =
        read_typed(data, &oxpath!("ui", "settings", "manual_model", "buffer")).unwrap_or_default();
    let trimmed = buffer.trim();

    match stage {
        "id" => {
            if trimmed.is_empty() {
                return Vec::new();
            }
            vec![
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(Value::String("ctx".into())),
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
        "ctx" => {
            let n: u32 = match trimmed.parse() {
                Ok(n) if n > 0 => n,
                _ => return Vec::new(),
            };
            vec![
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(Value::String("out".into())),
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
        "out" => {
            let out: u32 = match trimmed.parse() {
                Ok(n) if n > 0 => n,
                _ => return Vec::new(),
            };
            let id: String = read_typed(data, &oxpath!("ui", "settings", "manual_model", "staged_id"))
                .unwrap_or_default();
            let ctx: u32 = read_typed::<String>(data, &oxpath!("ui", "settings", "manual_model", "staged_ctx"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let account: String = read_typed(data, &oxpath!("ui", "settings", "manual_model", "account"))
                .unwrap_or_default();

            let comp = match ox_kernel::PathComponent::try_new(&account) {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            };

            // Read the existing catalog and append the new entry.
            let catalog_path = oxpath!("config", "gate", "accounts", comp, "models");
            let mut catalog: Vec<ox_gate::ModelInfo> =
                read_typed(data, &catalog_path).unwrap_or_default();
            catalog.push(ox_gate::ModelInfo {
                id: id.clone(),
                display_name: id,
                max_context_size: Some(ctx),
                max_output_tokens: Some(out),
                source: ox_gate::ModelInfoSource::UserEntered,
            });
            let catalog_value = match to_value(&catalog) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };

            // Write the catalog and clear the form state. Each form-state
            // path becomes a Null write to retire it.
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
                Write {
                    path: oxpath!("ui", "settings", "edit_mode"),
                    record: Record::parsed(Value::Bool(false)),
                },
            ]
        }
        _ => Vec::new(),
    }
}
```

- [ ] **Step 4: Verify + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add crates/ox-cli/src/settings/commands/edit.rs
git commit -m "feat(settings): manual model entry stage state machine

Three-stage form (id → ctx → out): each Enter validates and advances.
Final stage commits the assembled ModelInfo to the connection's
catalog with source=UserEntered and clears the form state. Empty
or non-numeric input rejects the stage advance without writing
anything (no partial state)."
```

### Task 6.5: Cancel command for the manual-model form

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/edit.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn manual_model_cancel_clears_form_without_writing_catalog() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("ctx".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "staged_id"),
            Value::String("custom".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit_mode"),
            Value::Bool(true),
        );
        let writes = run(&Cancel::new(), &mut snap);
        // No catalog write; all manual_model paths nulled; edit_mode off.
        assert!(!writes.iter().any(|w| w.path.to_string().starts_with("config/gate/accounts")));
        assert!(writes.iter().any(|w| w.path.to_string() == "ui/settings/manual_model/stage"));
        assert!(writes.iter().any(|w| w.path == oxpath!("ui", "settings", "edit_mode")));
    }
```

- [ ] **Step 2: Extend Cancel**

In `edit.rs::cancel` (or wherever the existing cancel logic lives), add:

```rust
fn cancel(data: &mut dyn Reader) -> Vec<Write> {
    let mut writes = Vec::new();
    // Manual-model form clears its own state additionally.
    if read_typed::<String>(data, &oxpath!("ui", "settings", "manual_model", "stage")).is_some() {
        for sub in ["account", "stage", "buffer", "staged_id", "staged_ctx"] {
            let comp = ox_kernel::PathComponent::try_new(sub).expect("identifier");
            writes.push(Write {
                path: oxpath!("ui", "settings", "manual_model", comp),
                record: Record::parsed(Value::Null),
            });
        }
    }
    // Existing cancel writes (edit_mode = false, edit_buffer cleared, etc).
    // ... append existing logic here ...
    writes
}
```

- [ ] **Step 3: Verify + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add crates/ox-cli/src/settings/commands/edit.rs
git commit -m "feat(settings): Cancel clears manual-model form state

Esc on the manual-model form nulls the form's account/stage/buffer/
staged_id/staged_ctx paths in addition to the regular edit-mode
cleanup. The user can abandon a partially-filled form without
leaving stale state behind."
```

### Slice 6 Definition of Done

- Pressing Enter on a "+ add model manually" row opens an inline three-field form.
- Three Enter presses advance through id → ctx → out, validating each input.
- Final Enter writes a new ModelInfo with `source: UserEntered` to the connection's catalog and clears the form.
- Esc abandons the form cleanly.
- All workspace tests green.

---

## 5. Slice 5 — Connection terminology + share-set indicator + joint form

**Outcome:** UI strings rename "Account(s)" → "Connection(s)". Each Connection row shows an inline indicator when its bound provider is shared with other Connections (`personal-anth · provider shared with 2 others`). Joint Add-Connection form on the `_new` overlay collects (name, provider-from-presets-or-existing, key) in one form rather than ping-ponging through separate flows.

### File Structure

| File | Change |
|---|---|
| `crates/ox-cli/src/settings/bootstrap.rs` | `populate_index_entries`: change "Accounts" / "Manage accounts and API keys." → "Connections" / "Manage connections (provider + account + key)." |
| `crates/ox-cli/src/settings/visible_rows.rs` | `append_account_rows`: include share-set indicator in `secondary` (e.g., `"shared with 2 others"`). Reuse the new `secondary` field landed in Slice 4. |
| `crates/ox-cli/src/settings/commands/account_model.rs` | New `accounts.fork_provider` command for users who want to break the share before editing endpoint/auth. |
| `crates/ox-cli/src/settings/renderers/index.rs` | Tree title rendering: change "Settings" + section-header strings as needed. |
| Help / hint files | Sweep "account" → "connection" in user-facing strings only. |

### Conventions specific to this slice

- **Code identifiers stay `account`.** Path components, struct field names, function names — none change. Only user-visible strings rename. This avoids touching every test fixture and broker reader.
- The share-set indicator computes `Vec<String>` of other accounts with the same `provider` field. When length > 0, append `"shared with N other{plural}"` to the row's secondary.
- "Fork provider" command duplicates the bound provider record under a new name (`{account}-fork`) and re-points the account to it. Available from any Connection row whose share-set is non-empty.

### Task 5.1: Rename UI strings in index entries

**Files:**
- Modify: `crates/ox-cli/src/settings/bootstrap.rs`

- [ ] **Step 1: Update strings**

In `populate_index_entries`:

```rust
    let accounts_entry = SettingsIndexEntry {
        id: "accounts".to_string(),    // <- id stays for stored-record compat
        label: "Connections".to_string(),
        description: "Manage connections (provider + account + key).".to_string(),
        target_cursor: oxpath!("settings", "accounts"),
        badge: BadgeSource::SubtreeCount(oxpath!("config", "gate", "accounts")),
    };
```

- [ ] **Step 2: Update existing tests that assert label text**

```bash
grep -n "\"Accounts\"\|label.*Accounts" crates/ox-cli/src/settings
```

For each match in test code, update the expected label.

- [ ] **Step 3: Verify + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add -u
git commit -m "feat(settings): rename Accounts index entry → Connections (UI string only)

The id, target_cursor, and badge SubtreeCount path stay as 'accounts'
to avoid disturbing every reader, fixture, and stored entry. Only
the user-facing label and description change. Subsequent commits
sweep secondary surfaces (help text, hint strings)."
```

### Task 5.2: Render share-set indicator in account-row secondary

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn account_row_secondary_indicates_shared_provider() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        // Three accounts, two share provider "anthropic", one uses "openai".
        write_account(&mut snap, "personal", "anthropic");
        write_account(&mut snap, "work", "anthropic");
        write_account(&mut snap, "lab", "openai");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let personal = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Account { name } if name == "personal"))
            .expect("personal row");
        assert_eq!(
            personal.secondary.as_deref(),
            Some("anthropic · shared with 1 other"),
            "row secondary must reflect provider sharing"
        );
        let lab = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Account { name } if name == "lab"))
            .expect("lab row");
        assert_eq!(lab.secondary.as_deref(), Some("openai"));
    }
```

`write_account` needs a `provider` argument; if the existing helper doesn't take one, extend it (or add a `write_account_with_provider`).

- [ ] **Step 2: Verify failure**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::account_row_secondary_indicates_shared_provider
```

- [ ] **Step 3: Implement**

In `append_account_rows`, before pushing each row, compute the provider's share count and assemble the secondary text:

```rust
fn append_account_rows(rows: &mut Vec<VisibleRow>, data: &mut dyn Reader, expanded: &[String]) {
    use ox_gate::AccountConfig;

    let names = child_names_under(data, "config/gate/accounts");
    // Pre-compute the provider-to-accounts map so the share-set lookup
    // is one pass, not N×N.
    let mut provider_users: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for n in &names {
        if let Ok(comp) = ox_kernel::PathComponent::try_new(n) {
            let acct: Option<AccountConfig> =
                read_typed(data, &oxpath!("config", "gate", "accounts", comp));
            if let Some(a) = acct {
                provider_users.entry(a.provider).or_default().push(n.clone());
            }
        }
    }

    for name in &names {
        let comp = match ox_kernel::PathComponent::try_new(name) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let acct: AccountConfig =
            read_typed(data, &oxpath!("config", "gate", "accounts", comp.clone()))
                .unwrap_or_else(|| AccountConfig {
                    provider: read_account_child_string_in_visible_rows(data, name, "provider")
                        .unwrap_or_else(|| "anthropic".to_string()),
                });

        let secondary = {
            let users = provider_users.get(&acct.provider);
            let other_count = users.map(|v| v.len().saturating_sub(1)).unwrap_or(0);
            if other_count > 0 {
                let plural = if other_count == 1 { "" } else { "s" };
                Some(format!(
                    "{} · shared with {} other{}",
                    acct.provider, other_count, plural
                ))
            } else {
                Some(acct.provider.clone())
            }
        };

        let path = row_path(&["settings", "accounts", &safe_component(name)]);
        let path_str = path_to_string(&path);
        let is_expanded = expanded.iter().any(|s| s == &path_str);
        rows.push(VisibleRow {
            path: path.clone(),
            depth: 1,
            label: name.clone(),
            secondary,
            badge: None,
            kind: RowKind::Account { name: name.clone() },
            expandable: true,
            expanded: is_expanded,
        });
        if is_expanded {
            append_account_field_rows(rows, data, name);
        }
    }
}
```

`read_account_child_string_in_visible_rows` is a local helper that reads `config/gate/accounts/{name}/provider` as a string — copy the implementation pattern from `read_account_child_string` in `account_model.rs`.

- [ ] **Step 4: Verify + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add crates/ox-cli/src/settings/visible_rows.rs
git commit -m "feat(settings): show provider share-set in Connection row secondary

Each Connection row's secondary text reads
'<provider> · shared with N other(s)' when the bound provider is
referenced by other Connections, or just '<provider>' when not.
Surfaces the multi-binding shape of the data so a user editing a
Connection can see whether changes will propagate."
```

### Task 5.3: `accounts.fork_provider` command

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn fork_provider_clones_record_and_repoints_account() {
        let mut snap = SettingsSnapshot::empty();
        // Two accounts share provider "anthropic".
        write_account(&mut snap, "personal", "anthropic");
        write_account(&mut snap, "work", "anthropic");
        write_provider(
            &mut snap,
            "anthropic",
            "https://api.anthropic.com",
            AuthScheme::XApiKey,
        );
        select_account(&mut snap, "personal");
        let writes = run_cmd(&AccountsForkProvider::new(), &mut snap);

        // Expect: write a new provider "personal-fork" + repoint personal's account.
        let provider_write = writes
            .iter()
            .find(|w| w.path.to_string() == "config/gate/providers/personal_fork")
            .expect("forked provider write");
        let pc: ProviderConfig =
            structfs_serde_store::from_value(provider_write.record.as_value().unwrap().clone())
                .unwrap();
        assert_eq!(pc.endpoint, "https://api.anthropic.com");

        let account_write = writes
            .iter()
            .find(|w| w.path.to_string() == "config/gate/accounts/personal")
            .expect("account repoint");
        let ac: AccountConfig =
            structfs_serde_store::from_value(account_write.record.as_value().unwrap().clone())
                .unwrap();
        assert_eq!(ac.provider, "personal_fork");
    }

    #[test]
    fn fork_provider_no_op_when_provider_not_shared() {
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "lone", "openai");
        write_provider(
            &mut snap,
            "openai",
            "https://api.openai.com",
            AuthScheme::BearerToken,
        );
        select_account(&mut snap, "lone");
        let writes = run_cmd(&AccountsForkProvider::new(), &mut snap);
        // No need to fork — the provider is already exclusive.
        assert!(writes.is_empty());
    }
```

- [ ] **Step 2: Verify failure + implement**

```rust
command! {
    struct_name: AccountsForkProvider,
    id: "accounts.fork_provider",
    title: "Fork Provider",
    description: "Clone the bound provider so this Connection no longer shares it with others. Edits to endpoint/auth/version then affect only this Connection.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_fork_provider(snap),
}

fn accounts_fork_provider(data: &mut dyn Reader) -> Vec<Write> {
    let selected = match read_selected_account(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let acct_comp = match ox_kernel::PathComponent::try_new(&selected) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let acct_path = oxpath!("config", "gate", "accounts", acct_comp.clone());
    let mut acct: AccountConfig = match read_typed(data, &acct_path) {
        Some(a) => a,
        None => return Vec::new(),
    };

    // Count other accounts that share this provider. If the count is
    // zero, the fork is a no-op (the provider is already exclusive to
    // this account).
    let names = crate::settings::renderers::util::child_names_under(data, "config/gate/accounts");
    let mut other_users = 0;
    for n in &names {
        if n == &selected {
            continue;
        }
        if let Ok(other_comp) = ox_kernel::PathComponent::try_new(n) {
            let other: Option<AccountConfig> =
                read_typed(data, &oxpath!("config", "gate", "accounts", other_comp));
            if let Some(o) = other {
                if o.provider == acct.provider {
                    other_users += 1;
                }
            }
        }
    }
    if other_users == 0 {
        return Vec::new();
    }

    // Read the currently-bound provider record. If it's missing, fork
    // a default-shaped one — better to surface the renamed provider with
    // sensible defaults than to silently no-op.
    let existing_provider_comp = match ox_kernel::PathComponent::try_new(&acct.provider) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let provider: ProviderConfig =
        read_typed(data, &oxpath!("config", "gate", "providers", existing_provider_comp))
            .unwrap_or_else(|| ProviderConfig {
                dialect: acct.provider.clone(),
                endpoint: String::new(),
                version: String::new(),
                auth: None,
            });

    // Forked name: "{account}_fork". Use safe_component to land at a
    // valid PathComponent. If the name collides, append a digit (rare;
    // guard for two-account fork sequences).
    let base = format!("{}_fork", selected);
    let forked_name = format!(
        "{}",
        crate::settings::visible_rows::safe_component(&base)
    );
    let forked_comp = match ox_kernel::PathComponent::try_new(&forked_name) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let forked_path = oxpath!("config", "gate", "providers", forked_comp);

    let provider_value = match to_value(&provider) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    acct.provider = forked_name;
    let acct_value = match to_value(&acct) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    vec![
        Write {
            path: forked_path,
            record: Record::parsed(provider_value),
        },
        Write {
            path: acct_path,
            record: Record::parsed(acct_value),
        },
    ]
}
```

`safe_component` may need to be re-exported from `visible_rows.rs` (currently private — make it `pub(crate)`).

- [ ] **Step 3: Bind to a key**

In `bindings.rs`, under `accounts_subtree` prefix, add:

```rust
    bind_prefix(
        reg,
        accounts_subtree.clone(),
        no_mods(),
        KeyCodeRepr::Char('f'),
        "accounts.fork_provider",
    );
```

`f` is unused in this scope; verify with the existing binding registration.

- [ ] **Step 4: Verify + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add -u
git commit -m "feat(settings): add accounts.fork_provider command

Clones the bound provider record under '{account}_fork' and repoints
the Connection at the new entry. Lets a user break a shared provider
binding before editing endpoint/auth/version when those edits should
be scoped to one Connection rather than propagating. Bound to 'f'
under settings/accounts."
```

### Task 5.4: Sweep remaining user-facing strings

**Files:** Help text, hint strings, any other user-visible "account" references.

- [ ] **Step 1: Find them**

```bash
grep -rn "\"[Aa]ccount" crates/ox-cli/src --include="*.rs" | grep -v "^.*://" | grep -v "// "
```

Filter the output by hand: keep matches in user-facing strings (titles, descriptions, hints, labels). Skip matches in code identifiers, comments, and broker paths.

- [ ] **Step 2: Update each match**

Common patterns:
- "Add Account" → "Add Connection"
- "Delete Account" → "Delete Connection"
- "account.test" command title "Test Connection" — already named correctly, skip
- "Manage accounts and API keys" — already replaced in Task 5.1, skip

For each remaining user-facing string match, change "account" → "Connection" in the *user-facing surface only*. Do NOT touch:
- `oxpath!("settings", "accounts", ...)` — broker path
- `RowKind::Account { name }` — Rust identifier
- `AccountConfig` / `read_selected_account` — Rust identifiers
- Comments — they're for developers, not users

- [ ] **Step 3: Verify + commit**

```bash
cargo fmt --all -- --check && cargo clippy -p ox-cli --all-targets -- -D warnings && cargo test -p ox-cli
git add -u
git commit -m "feat(settings): sweep user-facing 'account' strings → 'Connection'

Help text, hint strings, command titles, and overlay labels updated
to use the Connection terminology. Code identifiers, broker paths,
and developer comments retain 'account' — the data layer hasn't
changed, only the user's mental model of what each row represents."
```

### Slice 5 Definition of Done

- The Settings tree's first top-level entry reads "Connections (N)" instead of "Accounts (N)".
- Each Connection row's secondary shows the bound provider plus a share count when relevant.
- Pressing `f` on a Connection with a shared provider forks the provider so subsequent endpoint/auth edits are local.
- All user-facing strings consistently say "Connection"; code identifiers stay `account`.
- All workspace tests green.

---

## 6. Final verification (after all four slices)

- [ ] **Step 1: Full quality gates**

```bash
./scripts/quality_gates.sh
```

Expected: 15/15 pass.

- [ ] **Step 2: Confirm commit history is clean**

```bash
git log --oneline main..HEAD
```

Expected: each slice's commits appear in dependency order; no fixup/wip commits; no preexisting drift commits other than the necessary `chore(fmt)` ones.

- [ ] **Step 3: Roadmap status update**

Open `docs/superpowers/plans/2026-05-03-settings-connections-roadmap.md` and update §2 (Roadmap) to mark Slices 2–6 as completed. Commit:

```bash
git add docs/superpowers/plans/2026-05-03-settings-connections-roadmap.md
git commit -m "docs(plans): mark Settings redesign slices 2–6 complete in roadmap"
```

---

## What's still deferred after this plan

- Real `View::Table` variant in ox-view with column alignment and headers (current model rows use the existing `primary` + `secondary` slots).
- Pricing columns ($/in, $/out) — needs `ModelInfo` schema extension.
- `/` search/filter command over the Models table.
- Refresh-status indicator on the empty-state row ("refreshing…", "refresh failed: …") — requires reading `config/gate/accounts/{name}/refresh_status` per row.
- Auto-refresh on first expand / on key paste — the S-tier "live truth" hallmark from the persona discussion.
- Joint Add-Connection wizard (the `_new` overlay still ping-pongs through name → defaults; consolidating into one form is a separate slice).
- Full retirement of `config/gate/completions/primary` (kept for one release as the migration window per Slice 2 Task 2.4).

These are tracked as the "S-tier deltas" the persona conversations identified; each warrants its own brainstorm + plan when prioritized.
