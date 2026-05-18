# Compose-as-Whole-Form Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single-field compose mode with a `View::Form`-projected whole-form draft (Name, Protocol, Endpoint, Auth, Key) that fixes the shared-provider bug (each new account gets its own provider record) and turns the `add_connections_have_independent_providers` reproducer green.

**Architecture:** Compose draft state lives at `ui/settings/new_account/*` (scattered atoms). Validation is computed by the write commands and cached at `…/errors`. The page renderer projects state into a `View::Form` carrying typed field-intent (`FormValue::{Text,Selector,ReadOnly}`) consumed by the existing `render_form` translator. `AccountField` from `ox-types::settings` is reused as the field identity; visual position lives only in `View::Form.focused` at projection time.

**Spec:** `docs/superpowers/specs/2026-05-06-compose-as-whole-form-design.md`.

**Tech Stack:** Rust, ratatui (via `View::Form` → `render_form`), serde, insta snapshots.

---

## Task 1: Add `ValidationErrors` to ox-types::settings

**Files:**
- Modify: `crates/ox-types/src/settings.rs`

- [ ] **Step 1: Write the failing roundtrip test**

Add to the existing `#[cfg(test)] mod tests` block in `crates/ox-types/src/settings.rs`:

```rust
#[test]
fn validation_errors_roundtrips() {
    json_roundtrip(ValidationErrors::default());

    let mut e = ValidationErrors::default();
    e.name = Some("required".into());
    e.endpoint = Some("bad".into());
    json_roundtrip(e);
}

#[test]
fn validation_errors_for_field_returns_matching_slot() {
    let mut e = ValidationErrors::default();
    e.name = Some("required".into());
    e.protocol = Some("pick one".into());

    assert_eq!(e.for_field(AccountField::Name), Some("required"));
    assert_eq!(e.for_field(AccountField::Protocol), Some("pick one"));
    assert_eq!(e.for_field(AccountField::Endpoint), None);
}

#[test]
fn validation_errors_is_clean_when_all_none() {
    assert!(ValidationErrors::default().is_clean());

    let mut e = ValidationErrors::default();
    e.key = Some("required".into());
    assert!(!e.is_clean());
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-types validation_errors
```

Expected: FAIL with "cannot find type `ValidationErrors`".

- [ ] **Step 3: Add `ValidationErrors` near the existing `ValidationDiagnostics`**

Insert in `crates/ox-types/src/settings.rs` after the `ValidationDiagnostics` block:

```rust
/// Compose-mode validation results. Distinct from `ValidationDiagnostics`
/// because compose has no `computed_at_ms` cache-coherence concern and
/// uses a closed struct-of-`Option` shape for exhaustive `match` in
/// `for_field`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ValidationErrors {
    pub name:     Option<String>,
    pub protocol: Option<String>,
    pub endpoint: Option<String>,
    pub auth:     Option<String>,
    pub key:      Option<String>,
}

impl ValidationErrors {
    pub fn is_clean(&self) -> bool {
        self.name.is_none()
            && self.protocol.is_none()
            && self.endpoint.is_none()
            && self.auth.is_none()
            && self.key.is_none()
    }

    pub fn for_field(&self, field: AccountField) -> Option<&str> {
        match field {
            AccountField::Name     => self.name.as_deref(),
            AccountField::Protocol => self.protocol.as_deref(),
            AccountField::Endpoint => self.endpoint.as_deref(),
            AccountField::Auth     => self.auth.as_deref(),
            AccountField::Key      => self.key.as_deref(),
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-types validation_errors
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-types/src/settings.rs
git commit -m "types: add ValidationErrors record for compose-mode draft validation"
```

---

## Task 2: Add field metadata helpers and `FIELD_ORDER`

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn field_label_matches_variant() {
    use ox_types::settings::AccountField;
    assert_eq!(field_label(AccountField::Name), "Name");
    assert_eq!(field_label(AccountField::Protocol), "Protocol");
    assert_eq!(field_label(AccountField::Endpoint), "Endpoint");
    assert_eq!(field_label(AccountField::Auth), "Auth");
    assert_eq!(field_label(AccountField::Key), "Key");
}

#[test]
fn field_kind_separates_text_from_selector() {
    use ox_types::settings::AccountField;
    assert_eq!(field_kind(AccountField::Name), FieldKind::Text);
    assert_eq!(field_kind(AccountField::Endpoint), FieldKind::Text);
    assert_eq!(field_kind(AccountField::Key), FieldKind::Text);
    assert_eq!(field_kind(AccountField::Protocol), FieldKind::Selector);
    assert_eq!(field_kind(AccountField::Auth), FieldKind::Selector);
}

#[test]
fn field_state_subpath_matches_spec() {
    use ox_types::settings::AccountField;
    assert_eq!(field_state_subpath(AccountField::Name), "name");
    assert_eq!(field_state_subpath(AccountField::Protocol), "protocol");
    assert_eq!(field_state_subpath(AccountField::Endpoint), "endpoint");
    assert_eq!(field_state_subpath(AccountField::Auth), "auth");
    assert_eq!(field_state_subpath(AccountField::Key), "key");
}

#[test]
fn field_order_lists_every_variant_exactly_once() {
    use std::collections::HashSet;
    use ox_types::settings::AccountField;
    let seen: HashSet<_> = FIELD_ORDER.iter().copied().collect();
    assert_eq!(seen.len(), FIELD_ORDER.len(), "FIELD_ORDER has duplicates");
    for v in [
        AccountField::Name,
        AccountField::Protocol,
        AccountField::Endpoint,
        AccountField::Auth,
        AccountField::Key,
    ] {
        assert!(seen.contains(&v), "FIELD_ORDER missing {v:?}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib field_label_matches_variant field_kind_separates field_state_subpath field_order
```

Expected: FAIL with undefined identifiers.

- [ ] **Step 3: Implement**

Add to the top of `crates/ox-cli/src/settings/commands/account_model.rs` (above the existing `command!` blocks, after the imports):

```rust
use ox_types::settings::AccountField;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text,
    Selector,
}

pub(crate) fn field_label(f: AccountField) -> &'static str {
    match f {
        AccountField::Name     => "Name",
        AccountField::Protocol => "Protocol",
        AccountField::Endpoint => "Endpoint",
        AccountField::Auth     => "Auth",
        AccountField::Key      => "Key",
    }
}

pub(crate) fn field_kind(f: AccountField) -> FieldKind {
    match f {
        AccountField::Name | AccountField::Endpoint | AccountField::Key => FieldKind::Text,
        AccountField::Protocol | AccountField::Auth                     => FieldKind::Selector,
    }
}

pub(crate) fn field_state_subpath(f: AccountField) -> &'static str {
    match f {
        AccountField::Name     => "name",
        AccountField::Protocol => "protocol",
        AccountField::Endpoint => "endpoint",
        AccountField::Auth     => "auth",
        AccountField::Key      => "key",
    }
}

pub(crate) const FIELD_ORDER: [AccountField; 5] = [
    AccountField::Name,
    AccountField::Protocol,
    AccountField::Endpoint,
    AccountField::Auth,
    AccountField::Key,
];
```

(If `use ox_types::settings::AccountField` is already imported above, skip the duplicate.)

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib field_label_matches_variant field_kind_separates field_state_subpath field_order
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "compose: add per-field metadata helpers and FIELD_ORDER"
```

---

## Task 3: Add `focus_next` / `focus_prev` helpers

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn focus_next_walks_field_order() {
    use ox_types::settings::AccountField;
    let walk: Vec<_> = std::iter::successors(
        Some(AccountField::Name),
        |f| Some(focus_next(*f)),
    )
    .take(6)
    .collect();
    assert_eq!(
        walk,
        vec![
            AccountField::Name,
            AccountField::Protocol,
            AccountField::Endpoint,
            AccountField::Auth,
            AccountField::Key,
            AccountField::Name, // wraps
        ]
    );
}

#[test]
fn focus_prev_walks_field_order_reversed() {
    use ox_types::settings::AccountField;
    let walk: Vec<_> = std::iter::successors(
        Some(AccountField::Name),
        |f| Some(focus_prev(*f)),
    )
    .take(6)
    .collect();
    assert_eq!(
        walk,
        vec![
            AccountField::Name,
            AccountField::Key,
            AccountField::Auth,
            AccountField::Endpoint,
            AccountField::Protocol,
            AccountField::Name, // wraps
        ]
    );
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib focus_next_walks focus_prev_walks
```

Expected: FAIL with "cannot find function `focus_next`".

- [ ] **Step 3: Implement**

Add below `FIELD_ORDER` in `crates/ox-cli/src/settings/commands/account_model.rs`:

```rust
pub(crate) fn focus_next(field: AccountField) -> AccountField {
    let idx = FIELD_ORDER.iter().position(|f| *f == field).expect("variant in FIELD_ORDER");
    FIELD_ORDER[(idx + 1) % FIELD_ORDER.len()]
}

pub(crate) fn focus_prev(field: AccountField) -> AccountField {
    let idx = FIELD_ORDER.iter().position(|f| *f == field).expect("variant in FIELD_ORDER");
    FIELD_ORDER[(idx + FIELD_ORDER.len() - 1) % FIELD_ORDER.len()]
}
```

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib focus_next_walks focus_prev_walks
```

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "compose: add focus_next / focus_prev walking FIELD_ORDER"
```

---

## Task 4: Add validation functions

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn validate_compose_name_flags_empty_invalid_and_duplicate() {
    assert_eq!(validate_compose_name("", &[]), Some("required".into()));
    assert!(validate_compose_name("with space", &[])
        .unwrap()
        .contains("not a valid identifier"));
    assert!(validate_compose_name("foo", &["foo".into()])
        .unwrap()
        .contains("already exists"));
    assert_eq!(validate_compose_name("foo", &["bar".into()]), None);
    // Trim whitespace before checking
    assert_eq!(validate_compose_name("  foo  ", &["bar".into()]), None);
}

#[test]
fn validate_compose_protocol_requires_some() {
    assert!(validate_compose_protocol(None).is_some());
    assert!(validate_compose_protocol(Some("anthropic")).is_none());
}

#[test]
fn validate_compose_endpoint_requires_nonempty() {
    assert_eq!(validate_compose_endpoint(""), Some("required".into()));
    assert_eq!(validate_compose_endpoint("  "), Some("required".into()));
    assert_eq!(validate_compose_endpoint("https://x.example"), None);
}

#[test]
fn validate_compose_auth_requires_some() {
    use ox_gate::provider::AuthScheme;
    assert!(validate_compose_auth(None).is_some());
    assert!(validate_compose_auth(Some(&AuthScheme::XApiKey)).is_none());
}

#[test]
fn validate_compose_key_required_only_when_auth_requires_it() {
    use ox_gate::provider::AuthScheme;
    // No auth selected: key is irrelevant
    assert_eq!(validate_compose_key("", None), None);
    // Auth doesn't require key
    assert_eq!(validate_compose_key("", Some(&AuthScheme::None)), None);
    // Auth requires key, empty
    assert!(validate_compose_key("", Some(&AuthScheme::XApiKey)).is_some());
    // Auth requires key, non-empty
    assert_eq!(validate_compose_key("sk-...", Some(&AuthScheme::XApiKey)), None);
}

#[test]
fn validate_compose_draft_collects_all_errors() {
    use ox_gate::provider::AuthScheme;
    let errors = validate_compose_draft(
        "",         // name
        None,       // protocol
        "",         // endpoint
        None,       // auth
        "",         // key
        &[],        // existing accounts
    );
    assert!(errors.name.is_some());
    assert!(errors.protocol.is_some());
    assert!(errors.endpoint.is_some());
    assert!(errors.auth.is_some());
    // Key is not required when auth is None
    assert!(errors.key.is_none());

    let clean = validate_compose_draft(
        "my-account",
        Some("anthropic"),
        "https://api.example.com",
        Some(&AuthScheme::XApiKey),
        "sk-abc",
        &[],
    );
    assert!(clean.is_clean());
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib validate_compose
```

Expected: FAIL with undefined function names.

- [ ] **Step 3: Implement**

Add below `focus_prev` in `crates/ox-cli/src/settings/commands/account_model.rs`:

```rust
use ox_gate::provider::AuthScheme;
use ox_path::PathComponent;
use ox_types::settings::ValidationErrors;

pub(crate) fn validate_compose_draft(
    name: &str,
    protocol: Option<&str>,
    endpoint: &str,
    auth: Option<&AuthScheme>,
    key: &str,
    existing_accounts: &[String],
) -> ValidationErrors {
    ValidationErrors {
        name: validate_compose_name(name, existing_accounts),
        protocol: validate_compose_protocol(protocol),
        endpoint: validate_compose_endpoint(endpoint),
        auth: validate_compose_auth(auth),
        key: validate_compose_key(key, auth),
    }
}

pub(crate) fn validate_compose_name(name: &str, existing: &[String]) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some("required".into());
    }
    if PathComponent::try_new(trimmed).is_err() {
        return Some(format!("'{trimmed}' is not a valid identifier"));
    }
    if existing.iter().any(|n| n == trimmed) {
        return Some(format!("'{trimmed}' already exists"));
    }
    None
}

pub(crate) fn validate_compose_protocol(protocol: Option<&str>) -> Option<String> {
    if protocol.is_none() {
        Some("select a protocol".into())
    } else {
        None
    }
}

pub(crate) fn validate_compose_endpoint(endpoint: &str) -> Option<String> {
    if endpoint.trim().is_empty() {
        Some("required".into())
    } else {
        None
    }
}

pub(crate) fn validate_compose_auth(auth: Option<&AuthScheme>) -> Option<String> {
    if auth.is_none() {
        Some("select an auth scheme".into())
    } else {
        None
    }
}

pub(crate) fn validate_compose_key(key: &str, auth: Option<&AuthScheme>) -> Option<String> {
    match auth {
        Some(scheme) if scheme.requires_key() && key.trim().is_empty() => {
            Some("required for this auth scheme".into())
        }
        _ => None,
    }
}
```

If `PathComponent::try_new` lives at a different path, adjust the import. Run `cargo check` to confirm.

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib validate_compose
```

Expected: 6 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "compose: add per-field and full-draft validation"
```

---

## Task 5: Reshape `accounts.compose.open` for multi-field initialization

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`
- Modify: `crates/ox-cli/src/settings/bindings.rs` (rebind `a`)

- [ ] **Step 1: Identify current `accounts.add` / `accounts.compose.open` shape**

Grep for the keystroke binding:

```bash
grep -n "accounts\.add\|accounts\.compose\.open\|\"a\"" crates/ox-cli/src/settings/bindings.rs | head -10
```

Note the file & line where `a` is currently bound — that binding will move to `accounts.compose.open`.

- [ ] **Step 2: Write the failing test**

Add to `#[cfg(test)] mod tests`:

```rust
#[test]
fn accounts_compose_open_initializes_multi_field_draft() {
    use ox_types::settings::{AccountField, ValidationErrors};

    let snap = test_snapshot_with_no_accounts();
    let writes = accounts_compose_open(&snap);

    let active = writes_value(&writes, "ui/settings/new_account/active");
    assert_eq!(active, Some(json!(true)));

    let focused = writes_value(&writes, "ui/settings/new_account/focused_field");
    assert_eq!(focused, Some(json!("name")));

    // Empty buffers for the three text fields
    for sub in ["name", "endpoint", "key"] {
        let v = writes_value(&writes, &format!("ui/settings/new_account/{sub}"));
        assert_eq!(v, Some(json!("")), "field {sub}");
    }

    // None for the two selector fields
    for sub in ["protocol", "auth"] {
        let v = writes_value(&writes, &format!("ui/settings/new_account/{sub}"));
        assert_eq!(v, Some(serde_json::Value::Null), "field {sub}");
    }

    // Errors record present, all required fields flagged
    let errors = writes_value(&writes, "ui/settings/new_account/errors")
        .expect("errors written");
    let errors: ValidationErrors = serde_json::from_value(errors).unwrap();
    assert!(errors.name.is_some());
    assert!(errors.protocol.is_some());
    assert!(errors.endpoint.is_some());
    assert!(errors.auth.is_some());
    // No auth → no key required
    assert!(errors.key.is_none());

    // Old single-field buffer is NOT written (would be a stale signal)
    assert!(
        writes_value(&writes, "ui/settings/new_account/buffer").is_none(),
        "legacy buffer must not be written"
    );
}
```

Helper `writes_value` and `test_snapshot_with_no_accounts` may need to be added or located — search for similar helpers in the existing tests file and reuse the pattern.

- [ ] **Step 3: Run to verify failure**

```bash
cargo test -p ox-cli --lib accounts_compose_open_initializes
```

Expected: FAIL.

- [ ] **Step 4: Replace `accounts_compose_open` (or `accounts_add` body) implementation**

Replace the body in `account_model.rs`:

```rust
pub(crate) fn accounts_compose_open(snap: &Snapshot) -> Vec<Write> {
    let existing_accounts = snap.child_names_under(oxpath!("config", "gate", "accounts"));
    let errors = validate_compose_draft("", None, "", None, "", &existing_accounts);

    vec![
        Write { path: oxpath!("ui", "settings", "new_account", "active"),        record: Record::parsed(json!(true)) },
        Write { path: oxpath!("ui", "settings", "new_account", "focused_field"), record: Record::parsed(json!("name")) },
        Write { path: oxpath!("ui", "settings", "new_account", "name"),          record: Record::parsed(json!("")) },
        Write { path: oxpath!("ui", "settings", "new_account", "protocol"),      record: Record::parsed(Value::Null) },
        Write { path: oxpath!("ui", "settings", "new_account", "endpoint"),      record: Record::parsed(json!("")) },
        Write { path: oxpath!("ui", "settings", "new_account", "auth"),          record: Record::parsed(Value::Null) },
        Write { path: oxpath!("ui", "settings", "new_account", "key"),           record: Record::parsed(json!("")) },
        Write { path: oxpath!("ui", "settings", "new_account", "errors"),        record: Record::parsed(serde_json::to_value(errors).unwrap()) },
    ]
}
```

Rename the existing `accounts.add` command id to `accounts.compose.open` (or fold its body in if `accounts.compose.open` already exists). In the `command!` block, set:
- `id: "accounts.compose.open"`
- `struct_name: AccountsComposeOpen`
- `title: "New connection"`
- `cursor: Some(oxpath!("settings", "accounts"))`
- `run: |snap, _ctx| accounts_compose_open(snap)`

Update the binding in `crates/ox-cli/src/settings/bindings.rs` so `a` at `Prefix(settings/accounts)` invokes `accounts.compose.open`.

- [ ] **Step 5: Run to verify pass**

```bash
cargo test -p ox-cli --lib accounts_compose_open_initializes
```

Expected: PASS.

Run a quick full-package build to catch downstream breakage:

```bash
cargo check -p ox-cli
```

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs crates/ox-cli/src/settings/bindings.rs
git commit -m "compose: reshape open command to initialize multi-field draft"
```

---

## Task 6: Migrate dispatcher discriminator to `new_account/active`

**Files:**
- Modify: `crates/ox-cli/src/settings/dispatch.rs`

- [ ] **Step 1: Locate the compose-mode pass**

```bash
grep -n "new_account/buffer\|_compose_new_account\|compose_mode" crates/ox-cli/src/settings/dispatch.rs | head -20
```

Note where the discriminator is checked (it currently reads `new_account/buffer` as `Some(_)`).

- [ ] **Step 2: Write the failing test**

Add or update a dispatcher test confirming that the synthetic compose scope fires only when `new_account/active == true`:

```rust
#[test]
fn dispatcher_enters_compose_scope_when_active_is_true() {
    let snap = test_snapshot_with_writes(vec![
        ("ui/settings/new_account/active", json!(true)),
        ("ui/settings/new_account/focused_field", json!("name")),
        ("ui/settings/new_account/name", json!("")),
    ]);
    let scope = dispatcher_scope_for(&snap);
    assert_eq!(scope, Some("settings/_compose_new_account".into()));
}

#[test]
fn dispatcher_skips_compose_scope_when_active_absent() {
    let snap = test_snapshot_with_writes(vec![]);
    let scope = dispatcher_scope_for(&snap);
    assert_ne!(scope, Some("settings/_compose_new_account".into()));
}

#[test]
fn dispatcher_skips_compose_scope_when_legacy_buffer_alone() {
    // Legacy state must not trigger compose mode; only `active == true` does.
    let snap = test_snapshot_with_writes(vec![
        ("ui/settings/new_account/buffer", json!("partial")),
    ]);
    let scope = dispatcher_scope_for(&snap);
    assert_ne!(scope, Some("settings/_compose_new_account".into()));
}
```

If `dispatcher_scope_for` helper doesn't exist, expose a minimal test hook on the compose pass that returns the scope it chose given a snapshot, mirroring how the existing pending-delete / manual-model passes are tested.

- [ ] **Step 3: Run to verify failure**

```bash
cargo test -p ox-cli --lib dispatcher_enters_compose_scope dispatcher_skips_compose_scope
```

Expected: FAIL (current implementation keys on `buffer`).

- [ ] **Step 4: Update the discriminator**

In the dispatcher's compose-mode pass, replace:

```rust
let active = snap.get(&oxpath!("ui", "settings", "new_account", "buffer"))
    .and_then(|r| r.as_str())
    .is_some();
```

with:

```rust
let active = snap.get(&oxpath!("ui", "settings", "new_account", "active"))
    .and_then(|r| r.as_bool())
    .unwrap_or(false);
```

(Method names may differ — match the existing `.as_*` accessor style in the file.)

- [ ] **Step 5: Run to verify pass**

```bash
cargo test -p ox-cli --lib dispatcher_enters_compose_scope dispatcher_skips_compose_scope
```

Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/dispatch.rs
git commit -m "compose: switch dispatcher discriminator from buffer to active"
```

---

## Task 7: Reshape `accounts.compose.insert_char` for focused-text-field writes

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn compose_insert_char_appends_to_focused_text_field() {
    let snap = test_snapshot_with_compose_state(/*name=*/ "my", /*focused=*/ "name");
    let writes = accounts_compose_insert_char(&snap, &ctx_with_char('p'));

    assert_eq!(
        writes_value(&writes, "ui/settings/new_account/name"),
        Some(json!("myp"))
    );
    // Errors recomputed
    assert!(writes_value(&writes, "ui/settings/new_account/errors").is_some());
}

#[test]
fn compose_insert_char_noop_on_selector_focus() {
    let snap = test_snapshot_with_compose_state_focus("protocol");
    let writes = accounts_compose_insert_char(&snap, &ctx_with_char('p'));
    assert!(writes.is_empty(), "should be no-op on selector field");
}

#[test]
fn compose_insert_char_recomputes_errors_per_keystroke() {
    let snap = test_snapshot_with_compose_state(/*name=*/ "", /*focused=*/ "name");
    let writes = accounts_compose_insert_char(&snap, &ctx_with_char('f'));

    let errors: ValidationErrors = serde_json::from_value(
        writes_value(&writes, "ui/settings/new_account/errors").unwrap()
    ).unwrap();
    // After typing one valid char, name is no longer "required"
    assert_eq!(errors.name, None);
}
```

Helpers `test_snapshot_with_compose_state` etc. should set up the new state shape; build them by extending existing helpers.

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib compose_insert_char
```

Expected: FAIL (current impl writes to `buffer`).

- [ ] **Step 3: Replace `accounts_compose_insert_char`**

```rust
pub(crate) fn accounts_compose_insert_char(snap: &Snapshot, ctx: &CommandContext) -> Vec<Write> {
    let focused = read_focused_field(snap);
    if field_kind(focused) != FieldKind::Text {
        return vec![];
    }
    let ch = match ctx.last_char() {
        Some(c) => c,
        None => return vec![],
    };
    let subpath = field_state_subpath(focused);
    let path = oxpath!("ui", "settings", "new_account", subpath);
    let mut buf = snap.get(&path).and_then(|r| r.as_str()).unwrap_or("").to_string();
    buf.push(ch);

    let writes = vec![
        Write { path: path.clone(), record: Record::parsed(json!(buf)) },
    ];
    recompute_errors_writes(snap, focused, Some(&buf), writes)
}
```

Plus the supporting helpers (add once, reuse across subsequent tasks):

```rust
fn read_focused_field(snap: &Snapshot) -> AccountField {
    snap.get(&oxpath!("ui", "settings", "new_account", "focused_field"))
        .and_then(|r| serde_json::from_value::<AccountField>(r.json().clone()).ok())
        .unwrap_or(AccountField::Name)
}

/// Append a write of the recomputed validation errors. `override_field` and
/// `override_value` let the caller substitute the just-written value of one
/// field without round-tripping through the snapshot.
fn recompute_errors_writes(
    snap: &Snapshot,
    override_field: AccountField,
    override_value: Option<&str>, // text-field override; selectors use other path
    mut writes: Vec<Write>,
) -> Vec<Write> {
    let read_text = |f: AccountField| -> String {
        if f == override_field {
            if let Some(v) = override_value {
                return v.to_string();
            }
        }
        snap.get(&oxpath!("ui", "settings", "new_account", field_state_subpath(f)))
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string()
    };
    let read_protocol = || -> Option<String> {
        snap.get(&oxpath!("ui", "settings", "new_account", "protocol"))
            .and_then(|r| r.as_str())
            .map(|s| s.to_string())
    };
    let read_auth = || -> Option<AuthScheme> {
        snap.get(&oxpath!("ui", "settings", "new_account", "auth"))
            .and_then(|r| serde_json::from_value::<AuthScheme>(r.json().clone()).ok())
    };

    let existing = snap.child_names_under(oxpath!("config", "gate", "accounts"));
    let errors = validate_compose_draft(
        &read_text(AccountField::Name),
        read_protocol().as_deref(),
        &read_text(AccountField::Endpoint),
        read_auth().as_ref(),
        &read_text(AccountField::Key),
        &existing,
    );
    writes.push(Write {
        path: oxpath!("ui", "settings", "new_account", "errors"),
        record: Record::parsed(serde_json::to_value(errors).unwrap()),
    });
    writes
}
```

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib compose_insert_char
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "compose: route insert_char to focused text field, recompute errors"
```

---

## Task 8: Reshape `accounts.compose.delete_back`

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn compose_delete_back_pops_focused_text_field() {
    let snap = test_snapshot_with_compose_state(/*name=*/ "myacc", /*focused=*/ "name");
    let writes = accounts_compose_delete_back(&snap);
    assert_eq!(
        writes_value(&writes, "ui/settings/new_account/name"),
        Some(json!("myac"))
    );
}

#[test]
fn compose_delete_back_on_empty_is_noop() {
    let snap = test_snapshot_with_compose_state(/*name=*/ "", /*focused=*/ "name");
    let writes = accounts_compose_delete_back(&snap);
    // Empty buffer: nothing to pop, no writes.
    assert!(writes.is_empty());
}

#[test]
fn compose_delete_back_noop_on_selector_focus() {
    let snap = test_snapshot_with_compose_state_focus("protocol");
    let writes = accounts_compose_delete_back(&snap);
    assert!(writes.is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib compose_delete_back
```

Expected: FAIL.

- [ ] **Step 3: Replace `accounts_compose_delete_back`**

```rust
pub(crate) fn accounts_compose_delete_back(snap: &Snapshot) -> Vec<Write> {
    let focused = read_focused_field(snap);
    if field_kind(focused) != FieldKind::Text {
        return vec![];
    }
    let subpath = field_state_subpath(focused);
    let path = oxpath!("ui", "settings", "new_account", subpath);
    let buf = snap.get(&path).and_then(|r| r.as_str()).unwrap_or("");
    if buf.is_empty() {
        return vec![];
    }
    let mut new_buf = buf.to_string();
    new_buf.pop();

    let writes = vec![
        Write { path: path.clone(), record: Record::parsed(json!(new_buf)) },
    ];
    recompute_errors_writes(snap, focused, Some(&new_buf), writes)
}
```

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib compose_delete_back
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "compose: route delete_back to focused text field"
```

---

## Task 9: Update `accounts.compose.cancel` to clear the subtree

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn compose_cancel_writes_null_to_new_account_root() {
    let snap = test_snapshot_with_compose_state(/*name=*/ "partial", /*focused=*/ "name");
    let writes = accounts_compose_cancel(&snap);

    // Single Null write to the subtree root; Phase-8 cascade clears children.
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "settings", "new_account"));
    let record_value = serde_json::to_value(&writes[0].record).unwrap();
    assert_eq!(record_value, serde_json::Value::Null);
}
```

- [ ] **Step 2: Run to verify failure**

If the existing cancel writes to `new_account/buffer` it will fail this test.

```bash
cargo test -p ox-cli --lib compose_cancel_writes_null
```

- [ ] **Step 3: Replace the body**

In the `command!` block for `AccountsComposeCancel`:

```rust
run: |_snap, _ctx| vec![Write {
    path: oxpath!("ui", "settings", "new_account"),
    record: Record::parsed(Value::Null),
}],
```

Extract the body into a named function `accounts_compose_cancel(snap)` if a free-standing function makes the test simpler; either shape is fine, just stay consistent with the rest of the file.

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib compose_cancel_writes_null
```

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "compose: cancel writes Null at subtree root for cascade clear"
```

---

## Task 10: Add `accounts.compose.cycle_forward` / `cycle_back` and bindings

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn cycle_forward_picks_first_option_when_none_selected() {
    let snap = test_snapshot_with_compose_state_focus("protocol"); // no protocol set
    let writes = accounts_compose_cycle_forward(&snap);

    let protocol_value = writes_value(&writes, "ui/settings/new_account/protocol")
        .expect("protocol written");
    // First option in PROTOCOL_OPTIONS
    assert_eq!(protocol_value, json!(PROTOCOL_OPTIONS[0]));
}

#[test]
fn cycle_forward_advances_among_protocol_options() {
    let snap = test_snapshot_with_compose_protocol(PROTOCOL_OPTIONS[0]);
    let writes = accounts_compose_cycle_forward(&snap);

    assert_eq!(
        writes_value(&writes, "ui/settings/new_account/protocol"),
        Some(json!(PROTOCOL_OPTIONS[1]))
    );
}

#[test]
fn cycle_forward_wraps_protocol() {
    let last = PROTOCOL_OPTIONS.last().copied().unwrap();
    let snap = test_snapshot_with_compose_protocol(last);
    let writes = accounts_compose_cycle_forward(&snap);

    assert_eq!(
        writes_value(&writes, "ui/settings/new_account/protocol"),
        Some(json!(PROTOCOL_OPTIONS[0]))
    );
}

#[test]
fn cycle_back_retreats_among_auth_options() {
    use ox_gate::provider::AuthScheme;
    let snap = test_snapshot_with_compose_auth(AuthScheme::ALL[1]);
    let writes = accounts_compose_cycle_back(&snap);

    assert_eq!(
        writes_value(&writes, "ui/settings/new_account/auth")
            .and_then(|v| serde_json::from_value::<AuthScheme>(v).ok()),
        Some(AuthScheme::ALL[0])
    );
}

#[test]
fn cycle_noop_when_focused_on_text_field() {
    let snap = test_snapshot_with_compose_state_focus("name");
    assert!(accounts_compose_cycle_forward(&snap).is_empty());
    assert!(accounts_compose_cycle_back(&snap).is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib cycle_forward cycle_back cycle_noop
```

Expected: FAIL.

- [ ] **Step 3: Implement**

In `account_model.rs`:

```rust
pub(crate) const PROTOCOL_OPTIONS: &[&str] = &[
    "anthropic",
    "openai",
    // ... match the set already used by the real-account inline-edit selector
];

pub(crate) fn accounts_compose_cycle_forward(snap: &Snapshot) -> Vec<Write> {
    cycle_focused_selector(snap, /*forward=*/ true)
}

pub(crate) fn accounts_compose_cycle_back(snap: &Snapshot) -> Vec<Write> {
    cycle_focused_selector(snap, /*forward=*/ false)
}

fn cycle_focused_selector(snap: &Snapshot, forward: bool) -> Vec<Write> {
    let focused = read_focused_field(snap);
    if field_kind(focused) != FieldKind::Selector {
        return vec![];
    }
    let writes = match focused {
        AccountField::Protocol => cycle_protocol(snap, forward),
        AccountField::Auth => cycle_auth(snap, forward),
        _ => vec![],
    };
    recompute_errors_writes(snap, focused, None, writes)
}

fn cycle_protocol(snap: &Snapshot, forward: bool) -> Vec<Write> {
    let current: Option<String> = snap
        .get(&oxpath!("ui", "settings", "new_account", "protocol"))
        .and_then(|r| r.as_str())
        .map(|s| s.to_string());
    let next = cycle_str_options(PROTOCOL_OPTIONS, current.as_deref(), forward);
    vec![Write {
        path: oxpath!("ui", "settings", "new_account", "protocol"),
        record: Record::parsed(json!(next)),
    }]
}

fn cycle_auth(snap: &Snapshot, forward: bool) -> Vec<Write> {
    let current: Option<AuthScheme> = snap
        .get(&oxpath!("ui", "settings", "new_account", "auth"))
        .and_then(|r| serde_json::from_value::<AuthScheme>(r.json().clone()).ok());
    let next = cycle_enum_options(&AuthScheme::ALL, current, forward);
    vec![Write {
        path: oxpath!("ui", "settings", "new_account", "auth"),
        record: Record::parsed(serde_json::to_value(next).unwrap()),
    }]
}

fn cycle_str_options(options: &[&str], current: Option<&str>, forward: bool) -> String {
    match current.and_then(|c| options.iter().position(|o| *o == c)) {
        None => if forward { options[0].into() } else { options[options.len() - 1].into() },
        Some(idx) => {
            let step = if forward { 1 } else { options.len() - 1 };
            options[(idx + step) % options.len()].into()
        }
    }
}

fn cycle_enum_options<T: Copy + PartialEq>(options: &[T], current: Option<T>, forward: bool) -> T {
    match current.and_then(|c| options.iter().position(|o| *o == c)) {
        None => if forward { options[0] } else { options[options.len() - 1] },
        Some(idx) => {
            let step = if forward { 1 } else { options.len() - 1 };
            options[(idx + step) % options.len()]
        }
    }
}
```

Register the new commands with `command! { id: "accounts.compose.cycle_forward", ... }` and `cycle_back`.

In `bindings.rs`, at synthetic scope `settings/_compose_new_account`:
- `h` / `Left` → `accounts.compose.cycle_back`
- `l` / `Right` → `accounts.compose.cycle_forward`

(The exact `PROTOCOL_OPTIONS` set must mirror the existing inline-edit protocol selector. Locate it via `grep -n "anthropic\|openai" crates/ox-cli/src/settings/`.)

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib cycle_forward cycle_back cycle_noop
```

Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs crates/ox-cli/src/settings/bindings.rs
git commit -m "compose: add cycle_forward / cycle_back for selector fields"
```

---

## Task 11: Add `accounts.compose.focus_next` / `focus_prev` and bindings

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn focus_next_command_advances_focused_field() {
    let snap = test_snapshot_with_compose_state_focus("name");
    let writes = accounts_compose_focus_next(&snap);
    assert_eq!(
        writes_value(&writes, "ui/settings/new_account/focused_field"),
        Some(json!("protocol"))
    );
}

#[test]
fn focus_prev_command_retreats_focused_field() {
    let snap = test_snapshot_with_compose_state_focus("name");
    let writes = accounts_compose_focus_prev(&snap);
    // Wraps to Key
    assert_eq!(
        writes_value(&writes, "ui/settings/new_account/focused_field"),
        Some(json!("key"))
    );
}

#[test]
fn focus_change_does_not_touch_other_state() {
    let snap = test_snapshot_with_compose_state(/*name=*/ "abc", /*focused=*/ "name");
    let writes = accounts_compose_focus_next(&snap);
    // Only focused_field should change
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "settings", "new_account", "focused_field"));
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib focus_next_command focus_prev_command focus_change_does
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
pub(crate) fn accounts_compose_focus_next(snap: &Snapshot) -> Vec<Write> {
    let current = read_focused_field(snap);
    let next = focus_next(current);
    vec![Write {
        path: oxpath!("ui", "settings", "new_account", "focused_field"),
        record: Record::parsed(serde_json::to_value(next).unwrap()),
    }]
}

pub(crate) fn accounts_compose_focus_prev(snap: &Snapshot) -> Vec<Write> {
    let current = read_focused_field(snap);
    let prev = focus_prev(current);
    vec![Write {
        path: oxpath!("ui", "settings", "new_account", "focused_field"),
        record: Record::parsed(serde_json::to_value(prev).unwrap()),
    }]
}
```

Register `command! { id: "accounts.compose.focus_next", ... }` and `focus_prev`.

In `bindings.rs`, at `settings/_compose_new_account`:
- `Tab` / `Down` → `accounts.compose.focus_next`
- `Shift+Tab` / `Up` → `accounts.compose.focus_prev`

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib focus_next_command focus_prev_command focus_change_does
```

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs crates/ox-cli/src/settings/bindings.rs
git commit -m "compose: add focus_next / focus_prev with Tab navigation bindings"
```

---

## Task 11.5: Add namecode dep, AccountConfig.display_name, widen validator

**Prerequisite for T12.** Account identity becomes opaque: the user-typed display name is encoded via the `namecode` crate into a valid XID identifier used as the path component, while the original Unicode string is stored on `AccountConfig.display_name` for rendering. See spec §3 (Account identity).

**Files:**
- Modify: `crates/ox-cli/Cargo.toml` (add `namecode` dep)
- Modify: `crates/ox-gate/src/account.rs` (add `display_name` field)
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs` (widen `validate_compose_name`; update its tests)

- [ ] **Step 1: Add the namecode dep**

In `crates/ox-cli/Cargo.toml`, add to `[dependencies]`:

```toml
namecode = "0.1"
```

Run `cargo check -p ox-cli` to confirm the dep resolves.

- [ ] **Step 2: Extend `AccountConfig` with `display_name`**

Modify `crates/ox-gate/src/account.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountConfig {
    /// Name of the provider dialect (e.g. `"anthropic"`, `"openai"`).
    pub provider: String,
    /// User-typed display name (arbitrary Unicode). When `None`, renderers
    /// fall back to the path component. `#[serde(default)]` keeps old
    /// on-disk records loadable without migration.
    #[serde(default)]
    pub display_name: Option<String>,
}
```

Run `cargo check -p ox-gate -p ox-cli` to confirm the field addition compiles across the workspace.

- [ ] **Step 3: Write the widened validator's failing tests**

Replace the existing test `validate_compose_name_flags_empty_invalid_and_duplicate` in the `#[cfg(test)] mod tests` block of `account_model.rs` with:

```rust
#[test]
fn validate_compose_name_flags_empty_and_duplicate() {
    assert_eq!(validate_compose_name("", &[]), Some("required".into()));

    // Hyphenated names are now ACCEPTED (they get namecode-encoded at commit).
    assert_eq!(validate_compose_name("my-personal", &[]), None);

    // Arbitrary Unicode is accepted.
    assert_eq!(validate_compose_name("Personal 1", &[]), None);

    // Length cap.
    let long = "a".repeat(257);
    assert!(validate_compose_name(&long, &[]).unwrap().contains("too long"));

    // Duplicate check: `existing` holds path components (namecoded form).
    // "anthropic" → encodes to "anthropic" (already valid XID).
    assert!(validate_compose_name("anthropic", &["anthropic".into()])
        .unwrap()
        .contains("already exists"));

    // Duplicate check via encoding: "my-personal" namecodes to some encoded form.
    // If we already have that encoded form on disk, the proposal collides.
    let encoded = namecode::encode("my-personal");
    assert!(validate_compose_name("my-personal", &[encoded])
        .unwrap()
        .contains("already exists"));

    // Trim whitespace.
    assert_eq!(validate_compose_name("  foo  ", &[]), None);
}
```

- [ ] **Step 4: Run to verify failure**

```bash
cargo test -p ox-cli --lib validate_compose_name
```

Expected: FAIL (the current validator rejects "my-personal" via PathComponent::try_new).

- [ ] **Step 5: Widen the validator**

In `account_model.rs`, replace `validate_compose_name`:

```rust
pub(crate) fn validate_compose_name(name: &str, existing: &[String]) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some("required".into());
    }
    if trimmed.chars().count() > 256 {
        return Some("too long (max 256 chars)".into());
    }
    // `existing` holds on-disk path components (already namecoded).
    // Encode the proposed name and compare.
    let encoded = namecode::encode(trimmed);
    if existing.iter().any(|n| n == &encoded) {
        return Some(format!("'{trimmed}' already exists"));
    }
    None
}
```

Remove the `use ox_kernel::PathComponent;` import line if it's no longer referenced elsewhere in the file (grep first: `grep -n PathComponent crates/ox-cli/src/settings/commands/account_model.rs`).

- [ ] **Step 6: Update the aggregator's clean-case test**

The earlier `validate_compose_draft_collects_all_errors` test was changed from `"my-account"` → `"my_account"` to dodge the old PathComponent rejection. With the validator widened, you can switch it back to `"my-account"` (or any other hyphenated form). Update the test accordingly:

```rust
// In validate_compose_draft_collects_all_errors:
let clean = validate_compose_draft(
    "my-account",          // hyphen is now fine
    Some("anthropic"),
    "https://api.example.com",
    Some(&AuthScheme::XApiKey),
    "sk-abc",
    &[],
);
assert!(clean.is_clean());
```

- [ ] **Step 7: Run to verify pass**

```bash
cargo test -p ox-cli --lib validate_compose
cargo test -p ox-cli --lib                       # full lib suite — no regressions
cargo check -p ox-cli -p ox-gate
```

Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-cli/Cargo.toml crates/ox-gate/src/account.rs crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "namecode: widen compose-name validator; add AccountConfig.display_name"
```

(Three files because: Cargo.toml adds the dep, account.rs adds the field, account_model.rs widens the validator. Each is one logical change. Cargo.lock will also change — stage it too if your workflow tracks it; check `git status`.)

---

## Task 12: Reshape `accounts.compose.commit` for per-account provider records

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

This is the bug-fix task. The reproducer test
`add_connections_have_independent_providers` (commit `b89f3b6`) turns
green here.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn compose_commit_with_errors_is_noop() {
    let snap = test_snapshot_with_compose_state(/*name=*/ "", /*focused=*/ "name");
    let writes = accounts_compose_commit(&snap);
    assert!(writes.is_empty(), "commit must no-op when errors present");
}

#[test]
fn compose_commit_writes_per_account_provider_record_with_valid_xid_name() {
    use ox_gate::provider::AuthScheme;
    // "personal" is already a valid XID — namecode encodes it to itself.
    let snap = test_snapshot_with_compose_full_draft(
        /*name=*/ "personal",
        /*protocol=*/ "anthropic",
        /*endpoint=*/ "https://api.example.com",
        /*auth=*/ AuthScheme::XApiKey,
        /*key=*/ "sk-xxx",
    );
    let writes = accounts_compose_commit(&snap);

    // Account record at the per-account path (path_id == "personal")
    let acct = writes_value(&writes, "config/gate/accounts/personal")
        .expect("account record written");
    assert_eq!(acct["provider"], json!("personal"));
    assert_eq!(acct["display_name"], json!("personal"));

    // Provider record at the SAME name (not "anthropic")
    let provider = writes_value(&writes, "config/gate/providers/personal")
        .expect("provider record written");
    assert_eq!(provider["dialect"], json!("anthropic"));
    assert_eq!(provider["endpoint"], json!("https://api.example.com"));

    // No write to providers/anthropic — that's the bug.
    assert!(
        writes_value(&writes, "config/gate/providers/anthropic").is_none(),
        "compose commit must NOT touch the shared anthropic provider"
    );

    // Key written under per-account name
    let key = writes_value(&writes, "secret/keys/personal");
    assert!(key.is_some(), "api key written for x-api-key auth");

    // Compose state cleared
    let cleared = writes_value(&writes, "ui/settings/new_account");
    assert_eq!(cleared, Some(serde_json::Value::Null));

    // Focus moves to the new account (path_id form)
    let focused = writes_value(&writes, "ui/settings/focused")
        .expect("focused path written");
    assert!(
        focused.as_str().unwrap().ends_with("/personal"),
        "focused row should be the new account"
    );
}

#[test]
fn compose_commit_namecodes_non_xid_display_name() {
    use ox_gate::provider::AuthScheme;
    // "my-personal" is NOT a valid XID (hyphen).
    let snap = test_snapshot_with_compose_full_draft(
        /*name=*/ "my-personal",
        /*protocol=*/ "anthropic",
        /*endpoint=*/ "https://api.example.com",
        /*auth=*/ AuthScheme::XApiKey,
        /*key=*/ "sk-xxx",
    );
    let writes = accounts_compose_commit(&snap);

    let path_id = namecode::encode("my-personal");
    assert_ne!(path_id, "my-personal", "hyphen must force encoding");

    // Records land at the encoded path; display_name preserves the original.
    let acct_path = format!("config/gate/accounts/{path_id}");
    let acct = writes_value(&writes, &acct_path)
        .expect("account record at encoded path");
    assert_eq!(acct["provider"], json!(path_id));
    assert_eq!(acct["display_name"], json!("my-personal"));

    let provider_path = format!("config/gate/providers/{path_id}");
    assert!(writes_value(&writes, &provider_path).is_some());

    let key_path = format!("secret/keys/{path_id}");
    assert!(writes_value(&writes, &key_path).is_some());
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib compose_commit_with_errors compose_commit_writes_per_account
```

Expected: FAIL.

- [ ] **Step 3: Replace `accounts_compose_commit`**

```rust
pub(crate) fn accounts_compose_commit(snap: &Snapshot) -> Vec<Write> {
    let display_name = snap.get(&oxpath!("ui", "settings", "new_account", "name"))
        .and_then(|r| r.as_str()).unwrap_or("").trim().to_string();
    let protocol: Option<String> = snap.get(&oxpath!("ui", "settings", "new_account", "protocol"))
        .and_then(|r| r.as_str()).map(String::from);
    let endpoint = snap.get(&oxpath!("ui", "settings", "new_account", "endpoint"))
        .and_then(|r| r.as_str()).unwrap_or("").trim().to_string();
    let auth: Option<AuthScheme> = snap.get(&oxpath!("ui", "settings", "new_account", "auth"))
        .and_then(|r| serde_json::from_value::<AuthScheme>(r.json().clone()).ok());
    let key = snap.get(&oxpath!("ui", "settings", "new_account", "key"))
        .and_then(|r| r.as_str()).unwrap_or("").to_string();

    let existing = snap.child_names_under(oxpath!("config", "gate", "accounts"));
    let errors = validate_compose_draft(
        &display_name,
        protocol.as_deref(),
        &endpoint,
        auth.as_ref(),
        &key,
        &existing,
    );
    if !errors.is_clean() {
        return vec![];
    }

    let protocol = protocol.expect("validated Some");
    let auth = auth.expect("validated Some");

    // Namecode-encode the user-typed display name into an opaque, valid-XID
    // path component. Idempotent on already-valid input.
    let path_id = namecode::encode(&display_name);
    let path_component = PathComponent::try_new(&path_id)
        .expect("namecode::encode produces valid XID by construction");

    let acct = AccountConfig {
        provider: path_id.clone(),
        display_name: Some(display_name.clone()),
    };
    let provider = ProviderConfig {
        dialect: protocol.clone(),
        endpoint: endpoint.clone(),
        version: protocol_default_version(&protocol),
        auth: Some(auth.clone()),
    };

    let mut writes = vec![
        Write {
            path: oxpath!("config", "gate", "accounts", path_component.clone()),
            record: Record::parsed(serde_json::to_value(&acct).unwrap()),
        },
        Write {
            path: oxpath!("config", "gate", "providers", path_component.clone()),
            record: Record::parsed(serde_json::to_value(&provider).unwrap()),
        },
    ];

    if auth.requires_key() {
        writes.push(Write {
            path: oxpath!("secret", "keys", path_component.clone()),
            record: Record::parsed(json!(key.trim())),
        });
    }

    // Focus + expand the new account row (path_id form).
    writes.push(Write {
        path: oxpath!("ui", "settings", "focused"),
        record: Record::parsed(json!(format!("settings/accounts/{path_id}"))),
    });
    writes.push(expand_settings_account_writes(snap, &path_id));

    // Clear draft state via subtree cascade (Phase 8).
    writes.push(Write {
        path: oxpath!("ui", "settings", "new_account"),
        record: Record::parsed(Value::Null),
    });

    writes
}
```

Note that `oxpath!` accepts `PathComponent` expressions for runtime values. The exact macro syntax may vary — check existing call sites that pass runtime components and follow that pattern. If the macro requires `&PathComponent`, adjust accordingly.

`protocol_default_version` and `expand_settings_account_writes` likely exist
already — locate via grep and reuse. If `expand` doesn't exist, inline:

```rust
fn expand_settings_account_writes(snap: &Snapshot, name: &str) -> Write {
    let mut expanded: BTreeSet<String> = snap
        .get(&oxpath!("ui", "settings", "expanded"))
        .and_then(|r| serde_json::from_value(r.json().clone()).ok())
        .unwrap_or_default();
    expanded.insert("settings/accounts".into());
    expanded.insert(format!("settings/accounts/{name}"));
    Write {
        path: oxpath!("ui", "settings", "expanded"),
        record: Record::parsed(serde_json::to_value(expanded).unwrap()),
    }
}
```

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib compose_commit
```

Expected: PASS.

Run the reproducer:

```bash
cargo test -p ox-cli --test settings_e2e add_connections_have_independent_providers
```

Expected: **PASS** (was failing before this task).

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "compose: commit writes per-account provider record (fixes shared-provider bug)"
```

---

## Task 13: Update `IndexRenderer` to project `View::Form` + read display_name for account rows

**Files:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs` (View::Form projection)
- Modify: `crates/ox-cli/src/settings/visible_rows.rs` (account row label prefers `display_name`)

### Part A: Account-row label uses `display_name`

Account rows in the visible-rows projection currently use the path component (the on-disk key under `config/gate/accounts/`) as the row's `primary` label. After T11.5 introduced `AccountConfig.display_name`, the row should prefer the display name when present, falling back to the path component for legacy records that lack the field.

- [ ] **Step A1: Write the failing test**

In `crates/ox-cli/src/settings/visible_rows.rs` test module (or its inline tests — locate per existing convention), add:

```rust
#[test]
fn account_row_primary_uses_display_name_when_present() {
    // Seed a snapshot with one account whose AccountConfig.display_name = "My Personal".
    let snap = snapshot_with_account_record(
        /*path_component=*/ "personal",
        AccountConfig { provider: "personal".into(), display_name: Some("My Personal".into()) },
    );
    let rows = visible_rows_for_settings(&snap);
    let account_row = rows.iter().find(|r| r.focus_path_suffix() == Some("settings/accounts/personal"))
        .expect("account row present");
    assert_eq!(account_row.primary, "My Personal");
}

#[test]
fn account_row_falls_back_to_path_component_when_display_name_absent() {
    // Legacy record: display_name = None (or field absent in old serde).
    let snap = snapshot_with_account_record(
        /*path_component=*/ "anthropic",
        AccountConfig { provider: "anthropic".into(), display_name: None },
    );
    let rows = visible_rows_for_settings(&snap);
    let account_row = rows.iter().find(|r| r.focus_path_suffix() == Some("settings/accounts/anthropic"))
        .expect("account row present");
    assert_eq!(account_row.primary, "anthropic");
}
```

Use whatever helper convention exists in this file (`snapshot_with_account_record`, `visible_rows_for_settings`, etc. — adapt to the file's existing test helpers).

- [ ] **Step A2: Run to verify failure**

```bash
cargo test -p ox-cli --lib account_row_primary_uses_display_name account_row_falls_back
```

Expected: FAIL.

- [ ] **Step A3: Implement**

In `visible_rows.rs` where the account row's `primary` field is constructed (around line 149 currently, where `acct: AccountConfig` is read from data), replace the hardcoded `name` use with:

```rust
let primary = acct.display_name.clone().unwrap_or_else(|| name.clone());
```

Use `primary` for the `ListItem.primary` field of the account row.

- [ ] **Step A4: Run to verify pass**

```bash
cargo test -p ox-cli --lib account_row_primary_uses_display_name account_row_falls_back
cargo test -p ox-cli --lib  # full lib suite, no regressions
```

- [ ] **Step A5: Commit**

```bash
git add crates/ox-cli/src/settings/visible_rows.rs
git commit -m "render: account row uses display_name with path-component fallback"
```

### Part B: View::Form projection when compose is active

- [ ] **Step 1: Write the failing tests**

In the `#[cfg(test)] mod tests` block of `index.rs`:

```rust
#[test]
fn index_renderer_emits_frame_list_when_compose_inactive() {
    let snap = test_snapshot_at_accounts_page(); // no new_account/active
    let view = IndexRenderer::render(&snap);
    // Existing shape preserved
    match view {
        View::Frame { content, .. } => match *content {
            View::List { .. } => {}
            other => panic!("expected List, got {other:?}"),
        },
        other => panic!("expected Frame, got {other:?}"),
    }
}

#[test]
fn index_renderer_emits_frame_stack_form_list_when_compose_active() {
    let snap = test_snapshot_at_accounts_page_with_compose_active();
    let view = IndexRenderer::render(&snap);
    match view {
        View::Frame { content, .. } => match *content {
            View::Stack { dir: Direction::Vertical, children } => {
                assert_eq!(children.len(), 2);
                let (form, _form_sizing) = &children[0];
                let (list, _list_sizing) = &children[1];
                assert!(matches!(form, View::Form { .. }));
                assert!(matches!(list, View::List { .. }));
            }
            other => panic!("expected Stack, got {other:?}"),
        },
        other => panic!("expected Frame, got {other:?}"),
    }
}

#[test]
fn compose_form_has_one_row_per_field_in_order() {
    let snap = test_snapshot_at_accounts_page_with_compose_active();
    let view = IndexRenderer::render(&snap);
    let form = extract_form(view).expect("Form present");
    assert_eq!(form.rows.len(), FIELD_ORDER.len());
    for (i, field) in FIELD_ORDER.iter().enumerate() {
        assert_eq!(form.rows[i].label, field_label(*field));
        match (field_kind(*field), &form.rows[i].value) {
            (FieldKind::Text, FormValue::Text { .. }) => {}
            // Selectors on a freshly-opened form are ReadOnly("(not selected)")
            (FieldKind::Selector, FormValue::ReadOnly(_)) => {}
            (kind, value) => panic!("field {field:?} kind={kind:?} got value={value:?}"),
        }
    }
}

#[test]
fn compose_form_focused_index_tracks_focused_field() {
    use ox_types::settings::AccountField;
    for field in FIELD_ORDER {
        let snap = test_snapshot_at_accounts_page_compose_focused(field);
        let view = IndexRenderer::render(&snap);
        let form = extract_form(view).expect("Form present");
        let expected = FIELD_ORDER.iter().position(|f| *f == field);
        assert_eq!(form.focused, expected, "field {field:?}");
    }
}

#[test]
fn compose_form_threads_errors_into_form_rows() {
    let snap = test_snapshot_at_accounts_page_compose_with_errors(
        // Set name to "with space" → invalid identifier error
        ValidationErrors {
            name: Some("'with space' is not a valid identifier".into()),
            ..Default::default()
        },
    );
    let view = IndexRenderer::render(&snap);
    let form = extract_form(view).expect("Form present");
    let name_idx = FIELD_ORDER.iter().position(|f| *f == AccountField::Name).unwrap();
    assert!(form.rows[name_idx].error.is_some());
    assert!(form.rows[name_idx].error.as_deref().unwrap().contains("not a valid identifier"));
}

#[test]
fn key_field_renders_masked_text_value() {
    let snap = test_snapshot_at_accounts_page_compose_with_key("sk-secret");
    let view = IndexRenderer::render(&snap);
    let form = extract_form(view).expect("Form present");
    let key_idx = FIELD_ORDER.iter().position(|f| *f == AccountField::Key).unwrap();
    match &form.rows[key_idx].value {
        FormValue::Text { masked, .. } => assert!(*masked, "Key value must be masked"),
        other => panic!("expected Text, got {other:?}"),
    }
}
```

Helper `extract_form(view) -> Option<Form>` walks Frame → Stack → Form.

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p ox-cli --lib index_renderer_emits compose_form_
```

Expected: FAIL.

- [ ] **Step 3: Implement the projection**

In `crates/ox-cli/src/settings/renderers/index.rs`, where the accounts-page View is built, branch on `new_account/active`:

```rust
let compose_active = snap
    .get(&oxpath!("ui", "settings", "new_account", "active"))
    .and_then(|r| r.as_bool())
    .unwrap_or(false);

let inner = if compose_active {
    let form = compose_form_view(snap);
    let form_height = form_row_height(&form);
    View::Stack {
        dir: Direction::Vertical,
        children: vec![
            (form, Sizing::Fixed(form_height)),
            (View::List { items, selected }, Sizing::Fill),
        ],
    }
} else {
    View::List { items, selected }
};

View::Frame { title, title_right, content: Box::new(inner) }
```

Add `compose_form_view` (can live in the same module or in
`settings/commands/account_model.rs` — pick wherever helps cohesion best):

```rust
fn compose_form_view(snap: &Snapshot) -> View {
    let focused_field = read_focused_field(snap);
    let errors: ValidationErrors = snap
        .get(&oxpath!("ui", "settings", "new_account", "errors"))
        .and_then(|r| serde_json::from_value(r.json().clone()).ok())
        .unwrap_or_default();

    let rows: Vec<FormRow> = FIELD_ORDER
        .iter()
        .map(|f| project_field(snap, *f, errors.for_field(*f)))
        .collect();

    let focused_idx = FIELD_ORDER.iter().position(|f| *f == focused_field);
    View::Form { rows, focused: focused_idx }
}

fn project_field(snap: &Snapshot, field: AccountField, error: Option<&str>) -> FormRow {
    let label = field_label(field).to_string();
    let error = error.map(String::from);

    let (value, hint) = match field {
        AccountField::Name | AccountField::Endpoint => {
            let v = snap.get(&oxpath!("ui", "settings", "new_account", field_state_subpath(field)))
                .and_then(|r| r.as_str()).unwrap_or("").to_string();
            (text_value(v, /*masked=*/ false), None)
        }
        AccountField::Key => {
            let v = snap.get(&oxpath!("ui", "settings", "new_account", "key"))
                .and_then(|r| r.as_str()).unwrap_or("").to_string();
            (text_value(v, /*masked=*/ true), Some("required for x-api-key / bearer-token".into()))
        }
        AccountField::Protocol => {
            let current: Option<String> = snap
                .get(&oxpath!("ui", "settings", "new_account", "protocol"))
                .and_then(|r| r.as_str()).map(String::from);
            (selector_value(current.as_deref(), PROTOCOL_OPTIONS), None)
        }
        AccountField::Auth => {
            let current: Option<AuthScheme> = snap
                .get(&oxpath!("ui", "settings", "new_account", "auth"))
                .and_then(|r| serde_json::from_value::<AuthScheme>(r.json().clone()).ok());
            let auth_strs: Vec<String> = AuthScheme::ALL.iter().map(|a| a.to_string()).collect();
            let current_str = current.map(|a| a.to_string());
            let refs: Vec<&str> = auth_strs.iter().map(|s| s.as_str()).collect();
            (selector_value(current_str.as_deref(), &refs), None)
        }
    };

    FormRow { label, value, error, hint }
}

fn text_value(v: String, masked: bool) -> FormValue {
    let cursor = v.chars().count() as u32;
    FormValue::Text { value: v, cursor, masked }
}

fn selector_value(current: Option<&str>, options: &[&str]) -> FormValue {
    match current.and_then(|c| options.iter().position(|o| *o == c)) {
        None => FormValue::ReadOnly("(not selected)".into()),
        Some(idx) => FormValue::Selector {
            options: options.iter().map(|s| s.to_string()).collect(),
            current: idx,
        },
    }
}

fn form_row_height(view: &View) -> u16 {
    match view {
        View::Form { rows, .. } => {
            // Each row: 1 line + 1 line if error present.
            let lines: u16 = rows.iter()
                .map(|r| 1 + if r.error.is_some() { 1 } else { 0 })
                .sum();
            // Plus a header line (e.g., "+ New connection (Tab to navigate, Enter to create, Esc to cancel)")
            lines + 1
        }
        _ => 0,
    }
}
```

If `read_focused_field` is defined only in `account_model.rs`, make it
`pub(crate)` so the index renderer can call it.

- [ ] **Step 4: Run to verify pass**

```bash
cargo test -p ox-cli --lib index_renderer_emits compose_form_ key_field_renders
```

Expected: 6 tests pass.

Check the full crate still builds:

```bash
cargo check -p ox-cli
```

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/renderers/index.rs crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "compose: project draft state as View::Form when active"
```

---

## Task 14: Update E2E tests and verify reproducer; insta snapshots

**Files:**
- Modify: `crates/ox-cli/tests/settings_e2e.rs`
- Likely Create / update: snapshot files in `crates/ox-cli/tests/snapshots/`

- [ ] **Step 1: Replace the Phase-3 single-field replay test**

Find and rewrite the Phase-3 inline-create replay test:

```bash
grep -n "add_connection_inline_ghost_row\|add_account_create_flow" crates/ox-cli/tests/settings_e2e.rs | head -5
```

Replace `add_connection_inline_ghost_row_accepts_typing` (or whichever Phase-3 test names live there) with a new test that:

1. Navigates to Settings → Accounts.
2. Presses `a` to open compose.
3. Asserts the rendered View contains a `View::Form` with 5 rows + focused = Some(0).
4. Types "my-personal" (11 chars) → asserts Name row buffer.
5. Presses Tab → focused advances to Protocol.
6. Presses `l` → Protocol cycles to first option; assert.
7. Presses Tab → Endpoint focused.
8. Types "https://api.example.com" → Endpoint buffer asserted.
9. Tab → Auth.
10. `l` → Auth cycles to XApiKey.
11. Tab → Key.
12. Types "sk-test".
13. Enter → assert account materialized at `config/gate/accounts/<namecoded>`, provider at `config/gate/providers/<namecoded>`, key at `secret/keys/<namecoded>`, where `<namecoded> = namecode::encode("my-personal")`. Assert `AccountConfig.display_name == Some("my-personal")` on the materialized record.
14. Assert focused row is the new account (under the encoded path).

Use insta snapshots after each major interaction (open, after each Tab, after commit). Snapshot the visible View shape.

```rust
#[test]
fn add_connection_form_accepts_field_by_field_input() {
    let mut harness = SettingsHarness::new_at_accounts();

    harness.press_key("a");
    insta::assert_snapshot!("compose_open", harness.render());

    for c in "my-personal".chars() {
        harness.press_key(&c.to_string());
    }
    insta::assert_snapshot!("compose_typed_name", harness.render());

    harness.press_key("Tab");
    harness.press_key("l"); // Protocol → first option
    insta::assert_snapshot!("compose_protocol_selected", harness.render());

    harness.press_key("Tab");
    for c in "https://api.example.com".chars() {
        harness.press_key(&c.to_string());
    }

    harness.press_key("Tab");
    harness.press_key("l"); // Auth → XApiKey

    harness.press_key("Tab");
    for c in "sk-test".chars() {
        harness.press_key(&c.to_string());
    }

    insta::assert_snapshot!("compose_full_draft", harness.render());

    harness.press_key("Enter");

    // Account record materialized at the namecoded path.
    let path_id = namecode::encode("my-personal");
    let path_comp = PathComponent::try_new(&path_id).expect("namecode produces valid XID");
    let account = harness.snapshot().get(&oxpath!("config", "gate", "accounts", path_comp.clone()));
    assert!(account.is_some(), "account record at encoded path");
    let acct: AccountConfig = serde_json::from_value(account.unwrap().json().clone()).unwrap();
    assert_eq!(acct.display_name, Some("my-personal".to_string()));
    let provider = harness.snapshot().get(&oxpath!("config", "gate", "providers", path_comp.clone()));
    assert!(provider.is_some(), "provider record at encoded path");

    insta::assert_snapshot!("compose_after_commit", harness.render());
}
```

Adjust `SettingsHarness` / `press_key` / `render` to match the helpers
that exist today (look at neighboring E2E tests for the convention).

- [ ] **Step 2: Update `add_account_create_flow` if it survives**

If a generic create-flow test exists separately, update its expectations
for the new flow shape. If it duplicates the new test above, delete it.

- [ ] **Step 3: Confirm the reproducer still passes (sanity)**

```bash
cargo test -p ox-cli --test settings_e2e add_connections_have_independent_providers
```

Expected: still PASS (turned green in Task 12; this is the verification it didn't regress).

- [ ] **Step 4: Run the full E2E suite**

```bash
cargo test -p ox-cli --test settings_e2e
```

Expected: all tests pass; new snapshots written (review with `cargo insta review`).

- [ ] **Step 5: Accept the snapshots**

```bash
cargo insta review
```

Visually inspect each snapshot. Confirm:
- 5-row form renders with labels Name, Protocol, Endpoint, Auth, Key
- Errors appear inline next to invalid fields
- Focused row has a visible cursor / focus indicator
- Selectors show carousel when focused, current value when not
- Key field's value renders masked

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/tests/settings_e2e.rs crates/ox-cli/tests/snapshots/
git commit -m "test: E2E coverage for compose-as-whole-form flow"
```

---

## Final verification

Run the full crate test suite and confirm no regressions:

```bash
cargo test -p ox-cli
cargo test -p ox-types
cargo clippy -p ox-cli -- -D warnings
```

Confirm the bug-fix reproducer is green:

```bash
cargo test -p ox-cli --test settings_e2e add_connections_have_independent_providers
```

Confirm no stale references to the Phase-3 single-field buffer remain:

```bash
grep -rn "new_account/buffer" crates/ 2>/dev/null
```

Expected: no matches.

---

## Self-review checklist

- [x] **Spec coverage:** every section of the spec maps to a task — types (§3, T1), metadata (§4, T2-T3), validation (§5, T4), commands (§6, T5-T12), bindings (§7, integrated into T5/T10/T11), projection (§8, T13), commit (§9, T12), removals (§10, distributed), tests (§11, distributed + T14).
- [x] **Placeholders:** no "TBD" or "implement appropriately" left in any task.
- [x] **Type consistency:** `AccountField` (not `NewAccountField`) used throughout; `FIELD_ORDER` const referenced consistently; `ValidationErrors::for_field` signature is `(AccountField) -> Option<&str>` everywhere it appears.
- [x] **Bite-sized tasks:** each task is one logical change with its own test + commit.
- [x] **Failure-first TDD:** every task writes a failing test before implementation.
