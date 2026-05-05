# Settings Connections Redesign — Roadmap + Slice 1

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement Slice 1 task-by-task. Slices 2–6 require their own detailed plans before execution; this document captures their shape and the open questions blocking them.

**Goal:** Restructure the settings screen from the 2026-04-27 Accounts/Models layout to a Connections/Models two-tier model that honors multi-account, multi-provider, and gateway use cases. **Slice 1** ships first: kill the hardcoded Protocol carousel that silently overwrites custom-provider accounts (e.g. `LMStudio`, corp-gateway) with `anthropic`/`openai` when the user cycles.

**Architecture:** Keep the path-MVU primitive (`renderer = &Reader → View`, `command = &Reader → Vec<Write>`, accordion tree). The 2026-04-27 spec stays in force for the rendering machinery — only the *index entries*, the Protocol field semantics, and a small set of new typed records change. Each slice ships independently and produces working software on its own.

**Tech Stack:** Rust, tokio, ratatui, structfs broker, ox-gate types.

**Spec it builds on:** `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md` (path-MVU primitive, accordion tree, BadgeSource, subscription protocol). This plan revises §6.1 (index entries) and §5.6 (UI records), without touching §3 (the architectural primitive) or §5.8 (subscription types).

---

## 1. Why

The shipped 2026-04-27 design optimized for "two simple top-level rows" (Accounts, Models). Three problems surfaced once the design met real configs:

1. **Provider entries with no account binding are invisible.** A user with `[gate.providers.lm_studio]` in TOML but no account referencing it has unreachable data. The UI only walks `config/gate/accounts/*`.

2. **The Protocol carousel silently lies.** `PROTOCOL_OPTIONS = &["anthropic", "openai"]` is hardcoded (`crates/ox-cli/src/settings/commands/account_model.rs:439`). An account with `provider="LMStudio"` falls back to `position_of → 0` and cycling forward overwrites it to `openai`. The user has no signal this happened until their next request goes to the wrong endpoint.

3. **Models silently expands to nothing.** The Models row's expand toggles, but `append_model_rows` reads `config/gate/accounts/{name}/models` which is empty until `refresh_now` has been written. No empty-state row, no auto-fetch.

The redesign treats each `(provider, account, key)` bundle as a first-class **Connection**. A user can have many Connections targeting the same upstream API — `personal-anthropic`, `work-anthropic`, `proxy-anthropic-corp` — each with independent endpoint/key/catalog. Providers stay as a separate typed record (shareable across Connections), but are edited *through* a Connection rather than as a top-level browsable category.

The Models tier becomes a flat table over `(connection, model_id)` pairs with two single-keystroke flags per row: **Default** (multi-select, gates the kernel-side per-thread default-available set) and **Bootstrap** (single-select, the model the first turn uses).

---

## 2. Roadmap

Six slices, ordered by leverage. Each ships independently.

| # | Slice | Status |
|---|---|---|
| 1 | Dynamic Protocol options (kill hardcoded carousel) | **shipped** |
| 2 | Bootstrap rename (`gate/completions/primary` → `gate/completions/bootstrap`) + per-row toggle | **shipped** |
| 3 | `default_available: Vec<ModelKey>` record + per-row toggle | **shipped (UI half).** Enforcement gate deferred — see completion plan §1 ("Slice 3 Status") for the architectural blocker (no per-call model selection in `complete` tool today; gate would live in the harness, not kernel, when that lands). |
| 4 | Models flat table — empty-state row per connection + inline `ctx/out` metadata in `secondary` | **shipped.** Real `View::Table` variant (column alignment, headers, search) is a separate ox-view slice; current rows use existing `primary`+`secondary`+`badge` slots. |
| 5 | Connections terminology + share-set indicator + fork-provider command | **shipped.** Joint Add-Connection form (single-screen wizard) deferred — current `_new` overlay still ping-pongs through name → defaults. |
| 6 | Manual model entry (`+ add row`, `ModelInfoSource::UserEntered`) | **shipped.** |

**Slice 1** is fully specified below. **Slices 2–6** require their own detailed plans (see §5) and, for slices 2/3/6, answers to the open questions in §3.

---

## 3. Open Questions (block specific slices)

These are decisions the human partner must make before the affected slice can be planned in full. Each question's answer changes the implementation shape, not just a parameter.

### Q1 — Default-available scope (blocks Slice 3)

When a user un-checks **Default** on a `(connection, model_id)` row, that should:

- **(a) Kernel-side gate.** Block the kernel from accepting a tool call asking for that model. `config/gate/completions/default_available: Vec<ModelKey>` lives in the broker config namespace; ox-kernel reads it at thread spawn and enforces.
- **(b) UI-only filter.** Hide the row from any model-picker UI but accept any cataloged model the kernel sees. `ui/settings/default_available` lives in the UI namespace; the kernel never reads it.

If (a), Slice 3 touches ox-kernel. If (b), Slice 3 is UI-only.

### Q2 — Manual model entry minimum field set (blocks Slice 6)

When a user adds a row to the Models table for a Connection that can't enumerate (no `/models` endpoint, or refresh failed), what fields must they fill?

- Minimum to call: `id`, `max_output_tokens`.
- Minimum for context budgeter to work: `id`, `max_context_size`, `max_output_tokens`.
- Optional: pricing fields (per `crates/ox-gate/src/pricing.rs`), display name.

Default proposal: `id` + `max_context_size` + `max_output_tokens` required; pricing optional. Override?

### Q3 — Bootstrap on a failing connection (blocks Slice 2 UX)

A user marks `(work-anthropic, claude-sonnet-4)` as bootstrap. The latest `test_status` for `work-anthropic` is `Failed { reason: "401 invalid key" }`. Should the toggle:

- **Block** with an inline error ("connection's last test failed; fix the key first").
- **Allow with warning** ("connection failing — bootstrap will retry on next launch").

Lean: allow with warning. The user is in control; transient failures shouldn't lock them out.

---

## 4. Slice 1 — Dynamic Protocol Options

**Outcome:** The Protocol field's carousel pulls its options from `config/gate/providers/*` (built-in presets unioned with user-configured providers). The current value is always present in the option list. Cycling cannot silently overwrite a custom-provider account.

**Non-goals (deferred to later slices):**
- "+ New provider…" sentinel and the add-provider flow (Slice 5).
- Provider deletion or share-set indicator (Slice 5).
- Auth carousel changes (`AUTH_OPTIONS` is a fixed Rust enum and stays static).

### File Structure

| File | Change |
|---|---|
| `crates/ox-cli/src/settings/commands/account_model.rs` | Add `pub fn resolve_protocol_options(data, current) -> Vec<String>`; modify `selector_cycle_protocol_dir` to use it; remove `pub const PROTOCOL_OPTIONS`. |
| `crates/ox-cli/src/settings/renderers/index.rs` | Pre-compute protocol options in `render()`; pass into `selector_carousel_spans`; remove `PROTOCOL_OPTIONS` import. |
| `crates/ox-cli/tests/settings_e2e.rs` | Add one end-to-end test: TOML-loaded custom-provider account + cycle Protocol forward + assert provider is preserved/advanced honestly. |

No new files. No schema changes. No subscription changes.

### Conventions & gotchas

- `ox_gate::presets()` returns `&'static [Preset]` with `id` strings `"anthropic"`, `"openai"`, `""` (Custom). Filter `custom` out — Custom is a UI-only escape hatch, not a stored provider name.
- Ordering: presets first (in `presets()` declaration order), then user providers in lexicographic order (matches `child_names_under` output), then the current value if absent. This keeps the default cycle through `anthropic → openai` for vanilla configs unchanged.
- `child_names_under("config/gate/providers")` returns names directly; no `PathComponent::try_new` call needed — broker writes already validated.
- `selector_carousel_spans` lives in the renderer and currently has no access to the `Reader`. The fix is to pre-compute options once at the top of `IndexRenderer::render` (only for the focused row, only if it's a Protocol field) and thread the resulting `Vec<String>` into the closure. Do not refactor `selector_carousel_spans` to take `&mut dyn Reader` directly — that fights the borrow checker against the iteration.

---

### Task 1: Add `resolve_protocol_options` helper with unit tests

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing tests**

Add these tests to the existing `#[cfg(test)] mod tests` block in `account_model.rs`:

```rust
#[test]
fn resolve_protocol_options_lists_presets_first() {
    let mut snap = SettingsSnapshot::empty();
    let opts = resolve_protocol_options(&mut snap, "anthropic");
    assert_eq!(opts, vec!["anthropic".to_string(), "openai".to_string()]);
}

#[test]
fn resolve_protocol_options_appends_user_providers() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("config", "gate", "providers", "lm_studio", "dialect"),
        Value::String("openai".into()),
    );
    snap.insert(
        &oxpath!("config", "gate", "providers", "lm_studio", "endpoint"),
        Value::String("http://127.0.0.1:1234".into()),
    );
    let opts = resolve_protocol_options(&mut snap, "anthropic");
    assert_eq!(
        opts,
        vec![
            "anthropic".to_string(),
            "openai".to_string(),
            "lm_studio".to_string()
        ]
    );
}

#[test]
fn resolve_protocol_options_dedupes_user_provider_named_like_preset() {
    // A user provider literally named "anthropic" must not appear twice.
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("config", "gate", "providers", "anthropic", "dialect"),
        Value::String("anthropic".into()),
    );
    let opts = resolve_protocol_options(&mut snap, "anthropic");
    assert_eq!(opts, vec!["anthropic".to_string(), "openai".to_string()]);
}

#[test]
fn resolve_protocol_options_appends_current_when_absent() {
    // Account whose provider isn't in presets and isn't a configured provider
    // record either (an orphan binding). The current value must still appear.
    let mut snap = SettingsSnapshot::empty();
    let opts = resolve_protocol_options(&mut snap, "LMStudio");
    assert_eq!(
        opts,
        vec![
            "anthropic".to_string(),
            "openai".to_string(),
            "LMStudio".to_string()
        ]
    );
}

#[test]
fn resolve_protocol_options_does_not_append_empty_current() {
    let mut snap = SettingsSnapshot::empty();
    let opts = resolve_protocol_options(&mut snap, "");
    assert_eq!(opts, vec!["anthropic".to_string(), "openai".to_string()]);
}
```

You will need these test imports at the top of the test module — most are already present, but `Value` and `SettingsSnapshot` should be added if missing:

```rust
use crate::settings::snapshot::SettingsSnapshot;
use structfs_core_store::Value;
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p ox-cli --lib settings::commands::account_model::tests::resolve_protocol_options_
```

Expected: 5 tests fail with "cannot find function `resolve_protocol_options`".

- [ ] **Step 3: Implement `resolve_protocol_options`**

Add this function to `account_model.rs`, near the existing `PROTOCOL_OPTIONS` const (which stays for now — it's removed in Task 4):

```rust
/// Resolve the carousel options for the Protocol field.
///
/// Built-in presets first (declaration order), then user-configured
/// providers (lexicographic), then the current value if it isn't already
/// in either set. The current-value tail guarantees that cycling from a
/// custom provider visits every option without silently overwriting.
pub fn resolve_protocol_options(data: &mut dyn Reader, current: &str) -> Vec<String> {
    use crate::settings::renderers::util::child_names_under;

    let mut options: Vec<String> = ox_gate::presets()
        .iter()
        .filter(|p| !p.custom)
        .map(|p| p.id.to_string())
        .collect();

    let mut user = child_names_under(data, "config/gate/providers");
    user.sort();
    user.retain(|n| !options.contains(n));
    options.append(&mut user);

    if !current.is_empty() && !options.iter().any(|o| o == current) {
        options.push(current.to_string());
    }

    options
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p ox-cli --lib settings::commands::account_model::tests::resolve_protocol_options_
```

Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "feat(settings): add resolve_protocol_options helper

Returns the carousel option list for the Protocol field: built-in presets
first, user-configured providers second, the current value appended if
absent. Used in the next commit to replace the hardcoded PROTOCOL_OPTIONS
slice."
```

---

### Task 2: Convert `selector_cycle_protocol_dir` to use dynamic options

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs:458-498`

- [ ] **Step 1: Write the failing test**

Add to the existing test module in `account_model.rs`:

```rust
#[test]
fn cycle_protocol_forward_from_custom_provider_does_not_snap_to_anthropic() {
    // Regression for the silent-overwrite bug: an account whose provider
    // isn't in the preset list must, when cycled forward, advance to the
    // *next* option (which is itself wrapping to anthropic) — not silently
    // jump to position 0.
    //
    // Specifically: with options [anthropic, openai, LMStudio] and current
    // "LMStudio" at idx 2, forward cycle goes to idx 0 = "anthropic".
    // Without resolve_protocol_options, the old code computed idx as
    // position_of("LMStudio") in [anthropic, openai] = None → unwrap_or(0)
    // = 0, then forward = (0+1)%2 = 1 = "openai". The fix: cycling now
    // honors the current value's actual position.
    let mut snap = SettingsSnapshot::empty();
    let comp = ox_kernel::PathComponent::try_new("local").unwrap();
    snap.insert(
        &oxpath!("config", "gate", "accounts", comp.clone()),
        to_value(&AccountConfig {
            provider: "LMStudio".into(),
        })
        .unwrap(),
    );
    snap.insert(
        &oxpath!("ui", "settings", "accounts", "selected"),
        to_value(&Some("local".to_string())).unwrap(),
    );

    let writes = selector_cycle_protocol_dir(&mut snap, CycleDir::Forward);
    assert_eq!(writes.len(), 1);
    let written: AccountConfig =
        structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
    // With options [anthropic, openai, LMStudio], forward from idx 2
    // wraps to idx 0 = "anthropic".
    assert_eq!(written.provider, "anthropic");
}

#[test]
fn cycle_protocol_back_from_custom_provider_lands_on_previous_option() {
    let mut snap = SettingsSnapshot::empty();
    let comp = ox_kernel::PathComponent::try_new("local").unwrap();
    snap.insert(
        &oxpath!("config", "gate", "accounts", comp.clone()),
        to_value(&AccountConfig {
            provider: "LMStudio".into(),
        })
        .unwrap(),
    );
    snap.insert(
        &oxpath!("ui", "settings", "accounts", "selected"),
        to_value(&Some("local".to_string())).unwrap(),
    );

    let writes = selector_cycle_protocol_dir(&mut snap, CycleDir::Back);
    assert_eq!(writes.len(), 1);
    let written: AccountConfig =
        structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
    // With options [anthropic, openai, LMStudio], back from idx 2 = "openai".
    assert_eq!(written.provider, "openai");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p ox-cli --lib settings::commands::account_model::tests::cycle_protocol_
```

Expected: both new tests fail (the existing code uses `PROTOCOL_OPTIONS` and silently snaps).

- [ ] **Step 3: Replace `selector_cycle_protocol_dir` body**

Locate the existing function (currently at `account_model.rs:458-498`). Replace the body's option-resolution and indexing with calls to `resolve_protocol_options`:

```rust
fn selector_cycle_protocol_dir(data: &mut dyn Reader, dir: CycleDir) -> Vec<Write> {
    let selected = match read_selected_account(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let name_comp = match ox_kernel::PathComponent::try_new(&selected) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let acct_path = oxpath!("config", "gate", "accounts", name_comp);
    let mut acct: AccountConfig = read_typed(data, &acct_path).unwrap_or_else(|| AccountConfig {
        provider: read_account_child_string(data, &selected, "provider")
            .unwrap_or_else(|| "anthropic".to_string()),
    });

    let options = resolve_protocol_options(data, &acct.provider);
    if options.is_empty() {
        return Vec::new();
    }
    let idx = options
        .iter()
        .position(|o| o == &acct.provider)
        .unwrap_or(0);
    let next = match dir {
        CycleDir::Forward => options[(idx + 1) % options.len()].clone(),
        CycleDir::Back => options[(idx + options.len() - 1) % options.len()].clone(),
    };
    acct.provider = next;

    let value = match to_value(&acct) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "selector.cycle.protocol: failed to encode AccountConfig");
            return Vec::new();
        }
    };
    vec![Write {
        path: acct_path,
        record: Record::parsed(value),
    }]
}
```

- [ ] **Step 4: Run all tests in the module to verify nothing else broke**

```bash
cargo test -p ox-cli --lib settings::commands::account_model
```

Expected: all tests pass, including the two new cycle tests and every pre-existing test in the module.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "fix(settings): Protocol cycle no longer silently overwrites custom providers

selector_cycle_protocol_dir now resolves its option list from
resolve_protocol_options(). An account whose provider isn't in the preset
list (e.g. LMStudio, corp-gateway) cycles honestly through the full set
including its own current value."
```

---

### Task 3: Pass dynamic options into the renderer's carousel

**Files:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs` (the `IndexRenderer::render` method and the `selector_carousel_spans` function signature)

- [ ] **Step 1: Write the failing test**

Add to the existing test module in `index.rs`:

```rust
#[test]
fn focused_protocol_row_renders_custom_provider_in_carousel() {
    use ox_types::AccountField;

    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    // Account whose provider is not in the preset list.
    let comp = ox_kernel::PathComponent::try_new("local").unwrap();
    snap.insert(
        &oxpath!("config", "gate", "accounts", comp.clone()),
        to_value(&AccountConfig {
            provider: "LMStudio".into(),
        })
        .unwrap(),
    );
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        crate::settings::visible_rows::expanded_set_to_value(&[
            "settings/accounts".to_string(),
            "settings/accounts/local".to_string(),
        ]),
    );
    // Focus the Protocol field row.
    snap.insert(
        &oxpath!("ui", "settings", "focused_row"),
        crate::settings::commands::navigation::path_to_value(&oxpath!(
            "settings",
            "accounts",
            comp,
            "protocol"
        )),
    );

    let view = render(&mut snap);
    let (_title, items, selected) = assert_list(view);
    let i = selected.expect("a row should be selected");
    let primary_spans = items[i]
        .primary_spans
        .as_ref()
        .expect("focused Protocol row should render carousel spans");
    // The "current" span (bright) must contain the custom provider name —
    // the regression is that without dynamic options it would render
    // "anthropic" (idx 0 fallback) regardless of what the account stored.
    let joined: String = primary_spans.iter().map(|s| s.text.as_str()).collect();
    assert!(
        joined.contains("LMStudio"),
        "expected carousel to include 'LMStudio'; got {joined:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p ox-cli --lib settings::renderers::index::tests::focused_protocol_row_renders_custom_provider_in_carousel
```

Expected: fails. The current `selector_carousel_spans` looks up the value in `PROTOCOL_OPTIONS` only; not finding `LMStudio`, it falls back to idx 0 and renders `anthropic` as current.

- [ ] **Step 3: Update `selector_carousel_spans` to accept dynamic options**

Change the function signature in `index.rs` from:

```rust
fn selector_carousel_spans(
    row: &visible_rows::VisibleRow,
    indent: &str,
    glyph: &str,
) -> Option<Vec<Span>> {
```

to:

```rust
fn selector_carousel_spans(
    row: &visible_rows::VisibleRow,
    indent: &str,
    glyph: &str,
    protocol_options: &[String],
) -> Option<Vec<Span>> {
```

Inside the function, change the Protocol arm to use the new parameter:

```rust
        RowKind::AccountField {
            account,
            field: ox_types::AccountField::Protocol,
        } => {
            let _ = account;
            let value = row.label.split(": ").nth(1).unwrap_or("");
            let idx = protocol_options
                .iter()
                .position(|o| o == value)
                .unwrap_or(0);
            // The render() caller passes a non-empty list whenever the
            // focused row is Protocol; if it's empty here, that's a caller
            // bug and we'd rather render no carousel than panic.
            if protocol_options.is_empty() {
                return None;
            }
            // Convert &[String] → &[&str] for the shared formatting block
            // below. Local borrow keeps the lifetimes simple.
            let opts: Vec<&str> = protocol_options.iter().map(|s| s.as_str()).collect();
            let len = opts.len();
            let prev = opts[(idx + len - 1) % len];
            let current = opts[idx];
            let next = opts[(idx + 1) % len];
            let dim = Style {
                fg: None,
                bg: None,
                modifiers: ModifierSet { dim: true, ..ModifierSet::default() },
            };
            let bright = Style {
                fg: None,
                bg: None,
                modifiers: ModifierSet { bold: true, ..ModifierSet::default() },
            };
            return Some(vec![
                Span::plain(format!("{indent}{glyph}Protocol: ")),
                Span { text: format!("◂ {prev}  "), style: dim },
                Span { text: current.to_string(), style: bright },
                Span { text: format!("  {next} ▸"), style: dim },
            ]);
        }
```

The Auth arm (and the post-match shared formatting block, if any survives) keeps using `AUTH_DISPLAY` as a static slice. Since the Protocol arm now `return`s directly, the shared post-match formatting block is no longer reachable from Protocol — keep it for Auth.

If after this change the post-match block becomes Auth-only and trivially small, inline it into the Auth arm and delete the shared block. Either shape is acceptable; the code reviewer should pick the one with fewer lines.

- [ ] **Step 4: Pre-compute `protocol_options` in `IndexRenderer::render`**

In `index.rs`, near the top of `IndexRenderer::render` (after `let cursor = …; let selected = …;`), add:

```rust
let protocol_options: Vec<String> = selected
    .and_then(|i| rows.get(i))
    .filter(|r| matches!(
        &r.kind,
        crate::settings::visible_rows::RowKind::AccountField {
            field: ox_types::AccountField::Protocol,
            ..
        }
    ))
    .map(|r| {
        let current = r.label.split(": ").nth(1).unwrap_or("");
        crate::settings::commands::account_model::resolve_protocol_options(ctx.data, current)
    })
    .unwrap_or_default();
```

Then update the call site inside the `.map()` closure (currently `selector_carousel_spans(row, &indent, glyph)`) to:

```rust
selector_carousel_spans(row, &indent, glyph, &protocol_options)
```

The closure captures `protocol_options` by reference; the borrow checker is happy because `ctx.data` was only consumed during the pre-computation.

- [ ] **Step 5: Run the failing test, plus the full renderer test module**

```bash
cargo test -p ox-cli --lib settings::renderers::index
```

Expected: the new `focused_protocol_row_renders_custom_provider_in_carousel` passes; all pre-existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/renderers/index.rs
git commit -m "fix(settings): Protocol carousel renders custom providers honestly

IndexRenderer pre-computes the protocol option list once per frame from
resolve_protocol_options() and threads it into selector_carousel_spans.
A focused Protocol row whose value isn't a built-in preset (e.g. LMStudio,
corp-gateway) now appears in the carousel as the current option instead
of being silently rendered as 'anthropic'."
```

---

### Task 4: Remove the `PROTOCOL_OPTIONS` constant

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Search for remaining uses**

```bash
grep -rn "PROTOCOL_OPTIONS" /Users/alex/Devel/AdjectiveNoun/ox/crates --include="*.rs"
```

Expected after Tasks 1–3: only the `pub const PROTOCOL_OPTIONS: &[&str] = &["anthropic", "openai"];` declaration in `account_model.rs:439` (or thereabouts) remains. If `index.rs` or any other file still references it, that file's update from Task 3 was incomplete — go back and fix.

- [ ] **Step 2: Delete the constant**

Remove the line:

```rust
pub const PROTOCOL_OPTIONS: &[&str] = &["anthropic", "openai"];
```

- [ ] **Step 3: Verify the build**

```bash
cargo build -p ox-cli
```

Expected: clean build. If the compiler complains about an unresolved import or unused symbol, find the dangling reference and fix.

- [ ] **Step 4: Run the full settings test suite**

```bash
cargo test -p ox-cli --lib settings
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "refactor(settings): retire hardcoded PROTOCOL_OPTIONS constant

All call sites now resolve options dynamically via
resolve_protocol_options(); the static slice has no remaining users."
```

---

### Task 5: End-to-end test through the dispatch path

**Files:**
- Modify: `crates/ox-cli/tests/settings_e2e.rs`

The unit tests in Tasks 1–3 cover the helper, command, and renderer in isolation. This task adds one e2e test that exercises a real broker, a TOML-loaded account whose provider isn't a preset, and the full key-dispatch path through `tree.activate`. The test would have caught the silent-overwrite bug at integration boundary, not just at unit boundary.

- [ ] **Step 1: Read the existing e2e test fixture**

Read `crates/ox-cli/tests/settings_e2e.rs` end to end first — the test fixture conventions (BrokerStore setup, MockTransport, `send_key` calls) are non-obvious and the new test must match them. Look in particular for an existing test that exercises Protocol cycling; clone its scaffolding and modify the provider name and assertions.

- [ ] **Step 2: Write the failing test**

Append to `crates/ox-cli/tests/settings_e2e.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cycling_protocol_on_custom_provider_account_does_not_overwrite() {
    // Set up a broker with `local` account whose provider is `LMStudio`
    // (not a built-in preset). Press `l` to cycle Protocol forward.
    // Assert the new value is "anthropic" (the next option after LMStudio
    // in resolve_protocol_options' [anthropic, openai, LMStudio] list,
    // wrapping from idx 2). Critically, assert the value is NOT "openai" —
    // which is what the pre-fix bug produced (idx fallback to 0, then +1).

    let (broker, client) = build_broker_with_local_account("LMStudio").await;
    let registry = build_registries(&broker);

    // Focus the Protocol field row of the `local` account, then press `l`.
    seed_focused_row_protocol(&client, "local").await;

    let outcome = send_key(
        &client,
        &registry,
        &key(no_mods(), KeyCodeRepr::Char('l')),
    )
    .await;
    assert!(matches!(outcome, KeyDispatchOutcome::Dispatched { .. }));

    // Read back the persisted AccountConfig.
    let acct: AccountConfig = client
        .read_typed(&oxpath!(
            "config",
            "gate",
            "accounts",
            ox_kernel::PathComponent::try_new("local").unwrap()
        ))
        .await
        .unwrap()
        .expect("account written");
    assert_eq!(
        acct.provider, "anthropic",
        "expected forward cycle from LMStudio to wrap to anthropic, not silently snap to openai"
    );
}
```

You will need to factor or borrow these helpers from existing tests in the same file (do not duplicate inline if they already exist — match the file's conventions):

- `build_broker_with_local_account(provider: &str) -> (BrokerStore, ClientHandle)` — wires UiStore + ConfigStore + InputStore mounts; writes one account at `config/gate/accounts/local` with the given provider.
- `build_registries(broker) -> (BindingRegistry, CommandRegistry, RendererRegistry)` — registers all three day-one registries.
- `seed_focused_row_protocol(client, account_name)` — writes `ui/settings/focused_row` to point at the Protocol field of the named account.

If equivalents don't exist, write them as private helpers above the new test rather than inlining 60 lines of setup.

- [ ] **Step 3: Run the test to verify it fails**

```bash
cargo test -p ox-cli --test settings_e2e cycling_protocol_on_custom_provider
```

Expected: fails before Tasks 1–4 land; passes after.

- [ ] **Step 4: Run the test to verify it passes**

If you've executed Tasks 1–4 in order, this test should pass without code changes — the integration path inherits the unit-level fix. If it fails, the dispatch path is calling something that bypasses `selector_cycle_protocol_dir` (e.g. a stale binding registration) — diagnose that, do not modify the test.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/tests/settings_e2e.rs
git commit -m "test(settings): e2e regression for Protocol cycle on custom-provider account

Drives the full dispatch path (key → binding → command → broker write)
with a TOML-loaded account whose provider is not a built-in preset.
Ensures cycling doesn't silently snap the account to a preset value."
```

---

### Slice 1 Definition of Done

- `PROTOCOL_OPTIONS` const is deleted; no `grep PROTOCOL_OPTIONS crates/` matches remain.
- `cargo test -p ox-cli` is green.
- A user with `[gate.accounts.LMStudio] provider = "LMStudio"` in their TOML can press `h`/`l` on the Protocol row and the carousel cycles through `[anthropic, openai, LMStudio]` honestly.
- A user with no custom providers continues to see `[anthropic, openai]` and cycle behavior is unchanged.

---

## 5. Slices 2–6 (Follow-up Plans Required)

Each slice below ships independently and produces working software. Before execution, each needs its own bite-sized plan written in this same shape. Slices 2/3/6 also need the answer to their gating open question (§3).

### Slice 2 — Bootstrap rename + per-row toggle

**Outcome:** `config/gate/completions/primary` becomes `config/gate/completions/bootstrap` (same `CompletionRole` shape, clearer name). The Models tier gets a per-row Bootstrap toggle (`b` key) that's exclusive (toggling one clears any other).

**Touch points:**
- New typed read at `config/gate/completions/bootstrap`. During migration, kernel reads new path, falls back to legacy path, writes go to both. Legacy retired in a follow-up after one release.
- `crates/ox-cli/src/settings/commands/account_model.rs::models_set_primary` becomes `models_set_bootstrap`; writes to new path.
- Models row renderer gets a "B" column showing `●` for the row that is bootstrap.
- `BadgeSource::PrimaryReference` either stays (until Slice 4 retires the badge) or is renamed to `BootstrapReference`.

**Blocker:** Q3 — bootstrap-on-failed-test policy.

### Slice 3 — `default_available` record + per-row toggle

**Outcome:** New typed record `Vec<ModelKey>` controlling which `(connection, model_id)` pairs a fresh thread sees. Models tier gets a "D" column with multi-select toggle (`d` key).

**Touch points (if Q1=a, kernel-side gate):**
- New record at `config/gate/completions/default_available: Vec<ModelKey>` (or `ui/settings/default_available` if Q1=b).
- ox-kernel reads at thread spawn, gates the tool-callable model set.
- Renderer gets D column.
- Toggle command: `models.toggle_default` adds/removes the focused row's `ModelKey` from the set.

**Blocker:** Q1 — kernel-gate vs UI-filter.

### Slice 4 — Models becomes a flat table

**Outcome:** The Models index entry's children are no longer "expand to see this account's models" but a single flat table over all `(connection, model_id)` pairs across all accounts. Per-account drill-in (the current `append_model_rows` shape) is retired in favor of column-based filtering.

**Touch points:**
- `visible_rows::append_model_rows` rewritten to enumerate the union directly without per-account expansion.
- New columns: Connection, Model, Ctx, Out, $/in, $/out, D, B (the latter two depend on Slices 2 and 3).
- Search/filter (`/`) command added.
- Empty-catalog row replaced by a "no models — refresh connection X" row per connection.

**Blocker:** depends on Slice 2 (bootstrap column) and Slice 3 (default column) for the full column set; can land partial without them, with stub columns.

### Slice 5 — Connections (joint provider+account form)

**Outcome:** The Accounts index entry is renamed to Connections. Each Connection row jointly edits its `AccountConfig` and the `ProviderConfig` it points at. Adding a Connection asks "use existing provider, or fork?" (default: fork). Editing a shared provider warns inline ("affects N other connections — apply to all, or fork?"). Orphan provider entries (no Connection references them) appear as "Unbound" rows with an "Attach" action.

**Touch points:**
- `bootstrap.rs::populate_index_entries` rewrites the first entry's id/label.
- `visible_rows::append_account_rows` becomes `append_connection_rows` and adds the share-set indicator.
- New command `connections.fork_provider` for the on-edit-of-shared-provider branch.
- New visible row kind `Unbound { provider: String }` for orphan providers.

**Blocker:** none in principle. Requires the most UI surface area work of any slice.

### Slice 6 — Manual model entry

**Outcome:** Users can add a row to a Connection's catalog manually, for sources that don't enumerate (e.g. local servers without `/models`). New `ModelInfoSource::UserEntered` variant; "+ add row" affordance under each Connection's Models grouping in the table.

**Touch points:**
- New enum variant in `crates/ox-gate/src/known_family.rs` or wherever `ModelInfoSource` lives.
- New command: `models.add_manual` opens an inline edit form; commits write a new entry into the Connection's `Vec<ModelInfo>`.
- Renderer shows a provenance icon per row (`server`, `known-table`, `manual`).

**Blocker:** Q2 — minimum field set.

---

## 6. Notes for the Executing Engineer

- **Follow `git log` style.** Recent commits in this repo use `feat(settings): …`, `fix(settings): …`, `refactor(settings): …`. Match that.
- **Don't touch the legacy path during Slice 1.** The 2026-04-27 spec is still the source of truth for the rendering machinery; Slice 1 is a surgical fix to one carousel, not a redesign. Resist the urge to "while I'm here, also fix X." Each slice has its own slice.
- **Don't add comments explaining the redesign.** Per repo convention (`feedback_no_phase_or_pr_comments` in user memory), comments explain *why* a piece of code is the way it is, not which slice it shipped in. The `resolve_protocol_options` doc comment in Task 1 is fine — it explains the ordering invariant. A comment like `// Phase R3: dynamic protocol resolution` is not.
- **Don't run the binary interactively from a tool.** Per repo convention, interactive verification is the human's job. Unit and e2e tests are sufficient evidence the slice works.
