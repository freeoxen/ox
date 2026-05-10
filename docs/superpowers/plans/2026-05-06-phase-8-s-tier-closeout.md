# Phase 8: S-tier closeout — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four gaps that kept the substrate convergence at A+ instead of S: (1) replace `_ =>` wildcards in `RowKind` matches with explicit per-variant arms so the compiler enforces completeness; (2) introduce an `AccountName` newtype that lifts the PathComponent validation rule into the type system, preventing unvalidated account names from reaching boundary functions; (3) drop the redundant `safe_component` substitution for account names (PathComponent validation already gates everything; substitution can only harm by colliding real-real); (4) deduplicate the `banner_error` helper that has copies in `account_model.rs` and `edit.rs`.

**Architecture:** Four commits, one per gap. Each commit is mechanical and bounded; the compiler is the test for #1 and #2.

**Tech Stack:** Rust workspace; `ox-types` (new newtype + shared helper), `ox-cli` (everywhere account names flow + RowKind matches).

**Spec:** Closes out [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md). Not formally part of the spec but completes its intent.

---

## File Map

**Modify (across all four commits):**
- `crates/ox-cli/src/settings/commands/edit.rs` — `match row.kind` blocks at lines 181, 206, 291, 332 — make exhaustive; deduplicate `banner_error`.
- `crates/ox-cli/src/settings/commands/tree.rs` — `match &row.kind` at line 196 — make exhaustive.
- `crates/ox-cli/src/settings/renderers/index.rs` — `match &row.kind` at lines 343, 427 — make exhaustive.
- `crates/ox-cli/src/settings/commands/account_model.rs` — `match` blocks at lines 362, 383, 593, 1230 — make exhaustive; deduplicate `banner_error`; thread `AccountName` through key APIs.
- `crates/ox-cli/src/settings/visible_rows.rs` — `safe_component` no longer called for account-name paths in `row_path` callers; keep for model IDs with a doc comment explaining the asymmetry.
- `crates/ox-types/src/settings.rs` — add `AccountName` newtype; add a shared `banner_error` helper.

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section / Task-N comments.

---

## Task 1: Total dispatch on `RowKind`

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/edit.rs`, `commands/tree.rs`, `renderers/index.rs`, `commands/account_model.rs`.

The current `RowKind` variants are `Entry`, `Account`, `Model`, `AccountField`, `ModelField` (post-Phase 6). Every site that matches `&RowKind` should enumerate all five with explicit arms. Any future variant addition will then surface as a compile error at every dispatch site.

- [ ] **Step 1: Inventory matches**

```
grep -rn 'match.*\.kind\|match &.*\.kind\|match row.kind\|match r.kind' --include='*.rs' crates/ox-cli/src/
```

The expected sites (post-Phase 6):
- `edit.rs:181` — in `begin_edit_account_text`, matches AccountField.
- `edit.rs:206` — in `begin_edit_model_field_inner`, matches ModelField.
- `edit.rs:291` — in `insert_char`, accept rule.
- `edit.rs:332` — in `commit`, dispatch by row kind.
- `tree.rs:196` — in `activate`, dispatch by row kind.
- `index.rs:343` — in `selector_carousel_spans`, AccountField selectors.
- `index.rs:427` — in `decorate_row_label`, edit-mode label substitution.
- `account_model.rs:362` — in `selector_cycle_protocol` or similar, AccountField match.
- `account_model.rs:383` — sibling to above.
- `account_model.rs:593` — in `models_add_manual`, account extraction.
- `account_model.rs:1230` — in tests.

- [ ] **Step 2: Make each match exhaustive**

For each site, replace any `_ =>` wildcard with explicit per-variant arms. The behavior of the wildcard becomes the behavior of the previously-uncovered variants (likely `Vec::new()` / `None` / `return row.label.clone()` depending on context).

Example — `tree.rs::activate`'s leaf-row match (around line 193-220):

Before:
```rust
match &row.kind {
    RowKind::AccountField { field: AccountField::Name, .. } => Vec::new(),
    RowKind::AccountField { account, field: AccountField::Protocol } => { ... },
    // ... other AccountField variants ...
    RowKind::ModelField { .. } => super::edit::begin_model_field(data),
    RowKind::Entry { .. } | RowKind::Account { .. } | RowKind::Model { .. } => Vec::new(),
}
```

After: (no change needed if the existing arms already cover every variant; verify each match is exhaustive.)

The bulk of this task is verification, not transformation. The compiler will tell you if any variant is missed once you remove `_ =>` wildcards (if any exist).

- [ ] **Step 3: Build to confirm exhaustiveness**

```
cargo build -p ox-cli
```

Expected: PASS. Any error names a missed variant; add the explicit arm with the previously-implicit behavior.

- [ ] **Step 4: Run tests + clippy**

```
cargo test -p ox-cli --lib
cargo clippy -p ox-cli --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 5: Commit**

```
git add -u
git commit -m "refactor(settings): make every RowKind match exhaustive

Replace remaining \`_ =>\` wildcards in RowKind matches with explicit
per-variant arms. The compiler now enforces dispatch completeness:
adding a new RowKind variant produces an error at every dispatch
site instead of silently falling through.

No behavior change — every existing wildcard arm is preserved as
the explicit per-variant behavior."
```

---

## Task 2: `AccountName` newtype

**Files:**
- Modify: `crates/ox-types/src/settings.rs` — add the newtype.
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs` — `accounts_compose_commit` constructs an `AccountName` from the buffer; `read_selected_account` and similar helpers return `Option<AccountName>`.
- Modify: `crates/ox-cli/src/settings/commands/edit.rs` — sites that take an account name as `&str` accept `&AccountName` (or convert).

The newtype gives compile-time guarantees: a function taking `&AccountName` cannot be called with an unvalidated `String`. Internal uses (passing names through Vec<String> from `child_names_under`) stay as Strings; the type-safety boundary is at "user-supplied" or "selected" inputs.

- [ ] **Step 1: Add the newtype to `ox-types::settings`**

In `crates/ox-types/src/settings.rs`:

```rust
/// A validated account name. Internally a `String`, but constructed
/// only via `try_new` which enforces the same rules as
/// `ox_kernel::PathComponent::try_new`. Using this type at function
/// boundaries replaces "the caller validated this string somehow"
/// with "the type system proves this string was validated."
///
/// The wire format (when serialized via serde) is just the inner
/// `String` — `#[serde(transparent)]`. Callers reading from the
/// broker should use `AccountName::try_new` on the deserialized
/// string; mid-transition this lets reads be "validate at the
/// boundary" rather than threading the type through wire formats.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountName(String);

impl AccountName {
    pub fn try_new(s: impl Into<String>) -> Result<Self, AccountNameError> {
        let s = s.into();
        // Same rule as PathComponent: UAX#31 identifier.
        ox_kernel::PathComponent::try_new(&s)
            .map_err(|_| AccountNameError(s.clone()))?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for AccountName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AccountName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct AccountNameError(pub String);

impl std::fmt::Display for AccountNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid account name: '{}'", self.0)
    }
}

impl std::error::Error for AccountNameError {}
```

If `ox-types` cannot depend on `ox-kernel` (cycle), instead duplicate the validation rule's BODY here. Check the dependency graph: `ox-types` is the foundation; `ox-kernel` likely depends on `ox-types` or the other way around. If a cycle forms, put `AccountName` in `ox-kernel` next to `PathComponent`.

- [ ] **Step 2: Add roundtrip + validation tests**

```rust
#[cfg(test)]
mod account_name_tests {
    use super::*;

    #[test]
    fn try_new_accepts_valid_identifier() {
        assert!(AccountName::try_new("personal").is_ok());
        assert!(AccountName::try_new("anthropic_2").is_ok());
        assert!(AccountName::try_new("_dunder").is_ok()); // Phase 7 retired the leading-underscore ban
    }

    #[test]
    fn try_new_rejects_invalid_identifier() {
        assert!(AccountName::try_new("bad-name").is_err());
        assert!(AccountName::try_new("has space").is_err());
        assert!(AccountName::try_new("").is_err());
        assert!(AccountName::try_new("9starts_with_digit").is_err());
    }

    #[test]
    fn serde_roundtrip_is_transparent() {
        let name = AccountName::try_new("alpha").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, r#""alpha""#);
        let back: AccountName = serde_json::from_str(&json).unwrap();
        assert_eq!(back, name);
    }
}
```

- [ ] **Step 3: Thread through key boundaries in `account_model.rs`**

Update the high-leverage call sites:

- `accounts_compose_commit`: replace `let comp = match PathComponent::try_new(...)` with `let name = match AccountName::try_new(trimmed)`. Then derive `comp` from `name.as_str()` for path construction.

- `read_selected_account`: change return type from `Option<String>` to `Option<AccountName>`. The reader reads a `String` from the broker; converts via `AccountName::try_new(s).ok()`.

- Helpers like `account_path(name: &str)` accept `&AccountName` instead. Body uses `name.as_str()`.

Don't thread through every internal `&str` — focus on user-input and broker-read boundaries. Internal uses (e.g., temporary local strings constructed from already-validated names) stay as Strings.

- [ ] **Step 4: Build + tests**

```
cargo build -p ox-cli
cargo test -p ox-cli --lib
cargo test -p ox-types --lib
cargo clippy -p ox-cli --all-targets -- -D warnings
```

Expected: PASS. Any compile error means a call site needs `AccountName::try_new(s)` or the reverse conversion (`name.as_str()`).

- [ ] **Step 5: Commit**

```
git add -u
git commit -m "refactor(settings): AccountName newtype lifts validation into the type

Account names that pass user-input or broker-read boundaries now
carry their validation in their type. AccountName::try_new enforces
the same rule as PathComponent::try_new (UAX#31 identifier);
construction is the only path that can produce an AccountName.

Threaded through accounts_compose_commit (user input boundary) and
read_selected_account (broker boundary) plus the helpers that build
account-derived paths. Internal uses (temporary locals from
already-validated names) stay as Strings — the type-safety
boundary is where untrusted strings enter, not everywhere.

The compiler now catches functions that accept account names as
String when they should accept AccountName. Replaces a runtime
re-validation pattern with a compile-time guarantee."
```

---

## Task 3: Retire `safe_component` for account names

**File:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

`safe_component` substitutes non-PathComponent characters with underscores. For account names, this is redundant: every account name in the system passes `PathComponent::try_new`, so substitution does nothing. Worse, two real account names that DIFFER only in non-PathComponent characters (`bad-name` and `bad_name`) would BOTH pass `safe_component` and produce the same output, colliding in the display tree. Removing the call eliminates this latent risk and makes the display path bijective with the validated account name.

For model IDs (which are NOT validated through PathComponent — they come from API responses with hyphens, dots, etc.), `safe_component` stays.

- [ ] **Step 1: Find the account-name call sites**

```
grep -n 'safe_component' crates/ox-cli/src/settings/visible_rows.rs
```

Expected: hits for both account-name calls (in `append_account_rows`, `append_account_field_rows`, etc.) and model-id calls (in `append_model_rows`, `append_model_field_rows`).

- [ ] **Step 2: Drop the account-name calls**

For each `safe_component(account_name)` or `safe_component(name)` where the input is an account name (not a model id), replace with the bare name:

Before:
```rust
let path = row_path(&[
    "settings", "accounts", &safe_component(name),
]);
```

After:
```rust
let path = row_path(&[
    "settings", "accounts", name,
]);
```

`row_path` requires identifier-safe components. Since account names have already passed `PathComponent::try_new` (verified at the top of `append_account_rows` — invalid names continue the loop), passing the bare name is safe.

- [ ] **Step 3: Add a doc comment to `safe_component`**

Update the function's doc comment to clarify its remaining role:

```rust
/// Sanitize a model id for use as a path component. Model ids come from
/// API responses or user-entered manual entries; they are NOT
/// PathComponent-validated at write time, so they may contain
/// hyphens / dots / other characters the path validator rejects.
///
/// **Not used for account names.** Account names pass
/// `PathComponent::try_new` at every write boundary; their display-tree
/// paths use the bare validated name. Account names that DIFFERED only
/// in non-PathComponent characters would otherwise collide in the
/// display tree (e.g. `bad-name` and `bad_name` both → `bad_name`).
pub(crate) fn safe_component(s: &str) -> String { ... }
```

- [ ] **Step 4: Build + tests**

```
cargo build -p ox-cli
cargo test -p ox-cli --lib
```

Expected: PASS. Any test that asserted on `safe_component`-substituted account paths is dead — those substitutions never fire for valid account names.

- [ ] **Step 5: Commit**

```
git add -u
git commit -m "refactor(settings): drop safe_component for account names

Account names are PathComponent-validated at every write boundary
(the broker rejects non-identifier writes; the TOML loader's invalid
names are filtered out at the top of append_account_rows). Calling
safe_component on already-validated account names is a redundant
substitution that produces the same string in the common case but
collides distinct names in the edge case (e.g. \`bad-name\` and
\`bad_name\` both produce \`bad_name\` if both somehow reached the
substitution).

Bare account names are now used as path components in the display
tree, making display paths bijective with validated names. Model
ids continue to use safe_component because they are NOT
PathComponent-validated (they come from API responses with
hyphens etc.); the function's doc comment now states this
asymmetry explicitly."
```

---

## Task 4: Deduplicate `banner_error`

**Files:**
- Modify: `crates/ox-types/src/settings.rs` — add a shared `banner_error` constructor.
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs` — drop the local `banner_error`; use the shared helper.
- Modify: `crates/ox-cli/src/settings/commands/edit.rs` — drop the local `banner_error`; use the shared helper.

Both copies build a `GlobalBanner::Error` and wrap it in a `Write` to `ui/global/banner` with `now_ms`. Identical bodies. Lift to `ox-types::settings`.

- [ ] **Step 1: Add the shared helper**

In `crates/ox-types/src/settings.rs`:

```rust
impl GlobalBanner {
    /// Build a `GlobalBanner::Error` value with `set_at_ms` set to
    /// the current epoch millis. Used by CLI command surfaces that
    /// surface validation errors to the user (e.g. invalid account
    /// names, reserved-prefix collisions).
    pub fn error(message: impl Into<String>) -> Self {
        GlobalBanner::Error {
            message: message.into(),
            set_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }
}
```

This puts the helper on the type, not as a free function — discoverable via the type, no module path to import.

- [ ] **Step 2: Update `account_model.rs`'s `banner_error`**

In `crates/ox-cli/src/settings/commands/account_model.rs`, the local `banner_error` at line 558 currently constructs the GlobalBanner directly. Replace its body with a call to `GlobalBanner::error`:

Before:
```rust
fn banner_error(message: String) -> Write {
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

After (option A — keep wrapper, simplify body):
```rust
fn banner_error(message: String) -> Write {
    let banner = GlobalBanner::error(message);
    Write {
        path: oxpath!("ui", "global", "banner"),
        record: Record::parsed(to_value(&banner).unwrap()),
    }
}
```

After (option B — full inline at call sites, drop the wrapper):
```rust
// At each call site:
vec![Write {
    path: oxpath!("ui", "global", "banner"),
    record: Record::parsed(to_value(&GlobalBanner::error(format!(...))).unwrap()),
}]
```

Option A keeps the call-site terseness; option B removes one indirection. Pick whichever you prefer; document the choice.

- [ ] **Step 3: Update `edit.rs`'s `banner_error`**

Same as Step 2, applied to the local `banner_error` at line 253 in `crates/ox-cli/src/settings/commands/edit.rs`.

- [ ] **Step 4: Build + tests**

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 5: Commit**

```
git add -u
git commit -m "refactor(types): GlobalBanner::error constructor centralizes the helper

Two copies of \`banner_error\` lived in account_model.rs and edit.rs;
both built GlobalBanner::Error with identical bodies. Lift the
construction to GlobalBanner::error on the type itself. The local
wrappers in account_model.rs and edit.rs delegate; if they drift in
the future, they drift on top of a shared base.

No behavior change."
```

---

## Task 5: Final S-tier verification

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

- [ ] **Step 3: Verify gap closure**

```
# Total dispatch: any RowKind match with `_ =>` wildcard?
grep -rn '_ =>' crates/ox-cli/src/settings/ | grep -v 'test\|//\|comment'
```

Expected: hits should be limited to non-RowKind matches (e.g., `match path.components.len() { ... _ => ... }`). Manually verify each remaining `_ =>` is on a non-RowKind subject.

```
# AccountName threaded through?
grep -rn 'AccountName::try_new\|AccountName{' crates/ 2>/dev/null | head -10
```

Expected: hits in `accounts_compose_commit`, `read_selected_account`, and other key boundaries.

```
# safe_component for account names?
grep -n 'safe_component' crates/ox-cli/src/settings/visible_rows.rs
```

Expected: only model-id calls remain; the function's doc comment notes the asymmetry.

```
# banner_error duplication?
grep -rn 'fn banner_error' crates/ 2>/dev/null
```

Expected: zero or one hit (a thin wrapper if you went with option A in Task 4); never two.

- [ ] **Step 4: Sanity check the result**

The substrate convergence is now S-tier:
- ✅ Three architectural commitments enforced (writes-as-actions, modes-as-state, real-only display paths) — Phases 0–7.
- ✅ Total dispatch on `RowKind` — Phase 8 Task 1.
- ✅ `AccountName` newtype lifts validation into the type system — Phase 8 Task 2.
- ✅ `safe_component` no longer collision-risks account names — Phase 8 Task 3.
- ✅ `banner_error` deduplicated — Phase 8 Task 4.

If anything in the smoke-test surfaces a regression, file follow-ups; the substrate work itself is done.

---

## Self-review checklist

- [x] Every `RowKind` match site is exhaustive (Task 1).
- [x] `AccountName::try_new` enforces validation; threaded through user-input + broker-read boundaries (Task 2).
- [x] `safe_component` no longer called for account names; doc comment reflects model-id-only usage (Task 3).
- [x] `banner_error` deduplicated to `GlobalBanner::error` constructor (Task 4).
- [x] Workspace green + clippy clean + grep-verified (Task 5).
