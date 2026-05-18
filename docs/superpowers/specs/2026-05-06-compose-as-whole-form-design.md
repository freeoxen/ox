# Compose-mode as a whole-form draft

**Date:** 2026-05-06
**Status:** Design — pending implementation plan
**Crates touched:** `ox-cli` (renderer, dispatcher, commands, bindings, tests)
**Spec context:** Closes out the substrate convergence by replacing the
single-field name-only compose mode with a multi-field draft form
emitted as a `View::Form`; fixes the long-standing shared-provider
bug surfaced in reproducer test
`add_connections_have_independent_providers` (commit `b89f3b6`).

## 1. Summary

The current compose mode is a single-field text buffer that captures
just the account name. On Enter, it materializes an `AccountConfig {
provider: "anthropic".to_string() }` — every new account points at the
shared `config/gate/providers/anthropic` record, so editing one
connection's endpoint silently mutates every other connection that
defaulted to it. The user has to know to invoke `accounts.fork_provider`
to escape the shared default; in practice they don't, and connections
quietly contaminate each other.

This design replaces compose mode with a whole-form draft expressed
as a `View::Form` block above the Accounts list. When the user
presses `a`, the page renderer emits a five-row `View::Form` (Name,
Protocol, Endpoint, Auth, Key) alongside the existing accounts
`View::List`. Each `FormRow` carries its own validation error. The
user navigates between fields with Tab; text fields accept printable
input; selector fields accept `h`/`l` cycling. Enter validates and
materializes the account with a **per-account provider record**
(`provider: "<account_name>"`); Esc clears the draft.

The draft state lives at `ui/settings/new_account/*` (scattered
atoms, matching the `manual_model` convention). Validation is
computed inline by the compose-mode write commands and cached at
`ui/settings/new_account/errors`. The page renderer projects state
into a `View::Form` whose `FormRow.error` slot carries the cached
error; the existing `render_form` translator
(`crates/ox-cli/src/view_render.rs:146`) does the TUI rendering. Any
future platform-specific renderer pattern-matches the same
`View::Form` and emits its own widgets.

## 2. Goals & non-goals

### Goals

- Replace the single-field `new_account/buffer` compose mode with a
  whole-form draft (Name, Protocol, Endpoint, Auth, Key).
- Project the draft as a `View::Form` block; do not invent new
  renderer code. Reuse the `View::Form` / `render_form` plumbing
  that already exists.
- New accounts get per-account provider records by default;
  shared providers become opt-in via explicit user action later.
- Per-field validation errors are visible inline via
  `FormRow.error`.
- Validation is cached (computed on write, not on render).
- Cancel (Esc) clears the draft. Navigating out of the settings
  screen mid-draft does NOT clear; the draft persists at
  `ui/settings/new_account/*` and resumes on return.
- The reproducer test `add_connections_have_independent_providers`
  turns green.

### Account identity

Account identity is **opaque**, not user-typed. When materializing a new
account, the user-typed display name is encoded via
[`namecode`](https://docs.rs/namecode/0.1.1/namecode/) into a valid
XID identifier; that encoded form is the path component. The original
user-typed string is stored as `AccountConfig.display_name` for
rendering. Renderers read `display_name`; lookups use the encoded
path component. Namecode is a no-op for inputs that already pass
`is_xid_identifier`, so existing on-disk configs survive byte-for-byte
without migration.

`AccountConfig` gains an `Option<String> display_name` field with
`#[serde(default)]` — when absent on an old config record, the
renderer falls back to the path component as the display string.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountConfig {
    pub provider: String,
    #[serde(default)]
    pub display_name: Option<String>,
}
```

Why this shape:
- User-typed strings should not be required to be valid path components
  (the URL-slug-vs-id principle). The display name is arbitrary
  Unicode; the path is opaque.
- Existing valid-identifier account names (e.g., `"anthropic"`) encode
  to themselves; no on-disk change.
- Adding a future "rename display name" feature does not invalidate
  references (the path stays stable across display-name edits — if we
  later decide rename should keep the original path).

### Non-goals

- Auto-fork existing shared-provider configurations. Users who
  already have a config where multiple accounts point at
  `gate.providers.anthropic` keep that state; the existing
  `accounts.fork_provider` command (bound at the account row) is
  the migration path. The convergence work doesn't auto-migrate.
- Migrate inline field-edit on real accounts or manual-model
  creation onto `View::Form`. Both are natural follow-ups (the
  reified-by-intent shape works for them too), but they are
  separate refactors.
- URL parseability / reachability validation on Endpoint. Empty is
  the only structural error.
- Arrow-key cursor movement inside text fields. Input always
  appends; backspace always pops from the end. `FormValue::Text`
  carries `cursor: u32` but compose-mode sets it to
  `value.chars().count()` on every projection. Mid-buffer editing
  is a follow-up; none of the existing inline-edit inputs support
  it either.
- A reusable `Form` umbrella abstraction (FormField trait,
  FormDescriptor, etc.). `View::Form` is sufficient. If a second
  consumer (manual-model, inline edit) wants to share a layout
  descriptor later, we extract one then.

## 3. State shape

All under `ui/settings/new_account/*` (scattered atoms; substrate
convention from `manual_model`):

| Path | Type | Meaning |
|---|---|---|
| `…/active` | `bool` | Discriminator. `true` = compose mode active. Absent or `false` = inactive. |
| `…/name` | `String` | Account name buffer. Empty until typed. |
| `…/protocol` | `Option<String>` | Selected protocol/dialect. `None` = unselected. |
| `…/endpoint` | `String` | Endpoint URL buffer. Empty until typed. |
| `…/auth` | `Option<AuthScheme>` | Selected auth scheme. `None` = unselected. |
| `…/key` | `String` | API key buffer. Empty until typed. |
| `…/focused_field` | `AccountField` | Which field has keyboard focus. The enum variant is the durable identity; position in the visual layout is derived at projection time. |
| `…/errors` | `ValidationErrors` | Cached validation result. Computed on every input write. |

`AccountField` already exists in `ox-types::settings` with the
exact five variants (`Name`, `Protocol`, `Endpoint`, `Auth`, `Key`)
and is reused for compose. New type in `ox-types::settings`:

```rust
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

`AccountField` is the field identity. State, dispatch, and validation
all speak in `AccountField`. Position-as-index is computed once,
at projection time, when populating `View::Form.focused: Option<usize>`.

Why not reuse `ValidationDiagnostics` (which already uses `AccountField`
as keys)? `ValidationDiagnostics` carries `computed_at_ms` for
real-account edit caching coherence and is keyed by a `BTreeMap`;
compose validation is synchronous and has a fixed closed field set,
so the struct-of-`Option<String>` is a closer fit and gives
exhaustive `match` ergonomics in `for_field`.

## 4. Field metadata and visual order

The enum-as-identity carries variant; the visual order and the
compose-specific per-field metadata (text-vs-selector, state path
suffix, display label) live in `account_model.rs` as free functions
because `AccountField` is owned by `ox-types` and the orphan rule
forbids out-of-crate `impl`s.

In `crates/ox-cli/src/settings/commands/account_model.rs`:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind { Text, Selector }

fn field_label(f: AccountField) -> &'static str {
    match f {
        AccountField::Name     => "Name",
        AccountField::Protocol => "Protocol",
        AccountField::Endpoint => "Endpoint",
        AccountField::Auth     => "Auth",
        AccountField::Key      => "Key",
    }
}

fn field_kind(f: AccountField) -> FieldKind {
    match f {
        AccountField::Name | AccountField::Endpoint | AccountField::Key => FieldKind::Text,
        AccountField::Protocol | AccountField::Auth                     => FieldKind::Selector,
    }
}

fn field_state_subpath(f: AccountField) -> &'static str {
    match f {
        AccountField::Name     => "name",
        AccountField::Protocol => "protocol",
        AccountField::Endpoint => "endpoint",
        AccountField::Auth     => "auth",
        AccountField::Key      => "key",
    }
}

/// Visual order of fields in the compose form. Reordering this is
/// a pure cosmetic change — state, dispatch, and validation are
/// keyed on the enum variant, not on position.
const FIELD_ORDER: [AccountField; 5] = [
    AccountField::Name,
    AccountField::Protocol,
    AccountField::Endpoint,
    AccountField::Auth,
    AccountField::Key,
];

fn focus_next(field: AccountField) -> AccountField {
    let idx = FIELD_ORDER.iter().position(|f| *f == field).unwrap();
    FIELD_ORDER[(idx + 1) % FIELD_ORDER.len()]
}

fn focus_prev(field: AccountField) -> AccountField {
    let idx = FIELD_ORDER.iter().position(|f| *f == field).unwrap();
    FIELD_ORDER[(idx + FIELD_ORDER.len() - 1) % FIELD_ORDER.len()]
}
```

Each enum variant is fully self-describing (label, kind,
state path suffix). `FIELD_ORDER` is the only place the
visual sequence is encoded, and it's only consumed by the
projection and by `focus_next` / `focus_prev`.

## 5. Validation function

`crates/ox-cli/src/settings/commands/account_model.rs`:

```rust
fn validate_compose_draft(
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

fn validate_compose_name(name: &str, existing: &[String]) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some("required".into());
    }
    if trimmed.chars().count() > 256 {
        return Some("too long (max 256 chars)".into());
    }
    // `existing` holds the on-disk path components (already namecoded).
    // Encode the proposed name and compare against those.
    let encoded = namecode::encode(trimmed);
    if existing.iter().any(|n| n == &encoded) {
        return Some(format!("'{}' already exists", trimmed));
    }
    None
}

fn validate_compose_protocol(protocol: Option<&str>) -> Option<String> {
    if protocol.is_none() {
        return Some("select a protocol".into());
    }
    None
}

fn validate_compose_endpoint(endpoint: &str) -> Option<String> {
    if endpoint.trim().is_empty() {
        return Some("required".into());
    }
    None
}

fn validate_compose_auth(auth: Option<&AuthScheme>) -> Option<String> {
    if auth.is_none() {
        return Some("select an auth scheme".into());
    }
    None
}

fn validate_compose_key(key: &str, auth: Option<&AuthScheme>) -> Option<String> {
    match auth {
        Some(scheme) if scheme.requires_key() && key.trim().is_empty() => {
            Some("required for this auth scheme".into())
        }
        _ => None,
    }
}
```

`existing_accounts` is read from `child_names_under("config/gate/accounts")`
once per compute, so the user gets a name-collision error before they
press Enter.

## 6. Commands

In `crates/ox-cli/src/settings/commands/account_model.rs`. Each
input-mutating command:
1. Reads current state from the broker.
2. Mutates the relevant field.
3. Recomputes validation.
4. Writes the field + validation in one batch.

| Command id | Purpose |
|---|---|
| `accounts.compose.open` | Initialize the draft: active=true, all fields empty/None, focused_field=Name, errors=full validation (everything required → multiple errors). |
| `accounts.compose.insert_char` | If `field_kind(focused_field) == Text`, append the just-pressed char to that field's buffer; recompute validation. No-op on Selector. |
| `accounts.compose.delete_back` | If `field_kind(focused_field) == Text`, pop last char; recompute validation. No-op on Selector. |
| `accounts.compose.cycle_forward` / `cycle_back` | If `field_kind(focused_field) == Selector`, advance/retreat the selected option for that field; recompute validation. First press on a `None` selector picks options[0] / options[last]. No-op on Text. |
| `accounts.compose.focus_next` / `focus_prev` | Walk `FIELD_ORDER` forward/back from the current variant. No state change beyond `focused_field`. |
| `accounts.compose.commit` | If errors clean, write account + provider + key, clear `ui/settings/new_account/*`, set focused row to the new account, expand it. If errors dirty, no-op (errors already visible). |
| `accounts.compose.cancel` | Null-write to `ui/settings/new_account`. Phase-8 store cascade clears the whole subtree in one operation. |

All commands read/write `focused_field` as `AccountField`; the
position-as-index never appears outside the projection.

The existing `selector_cycle_protocol` / `selector_cycle_auth`
commands (for real-account inline edits) are NOT reused — those
write to `config/gate/providers/<provider>/auth`. The compose-mode
variants write to `ui/settings/new_account/{protocol,auth}`.

## 7. Bindings

In `crates/ox-cli/src/settings/bindings.rs`:

- `Prefix(settings/accounts)` keystroke `a` → `accounts.compose.open`.
- At synthetic scope `settings/_compose_new_account` (the dispatcher
  enters this scope when `ui/settings/new_account/active == true`):
  - All printable ASCII (per the existing `register_text_editing` helper) → `accounts.compose.insert_char`.
  - `Backspace` → `accounts.compose.delete_back`.
  - `Tab` / `Down` → `accounts.compose.focus_next`.
  - `Shift+Tab` / `Up` → `accounts.compose.focus_prev`.
  - `h` / `Left` → `accounts.compose.cycle_back`.
  - `l` / `Right` → `accounts.compose.cycle_forward`.
  - `Enter` → `accounts.compose.commit`.
  - `Esc` → `accounts.compose.cancel`.

The dispatcher's compose-mode pass discriminator changes from
`new_account/buffer == Some(_)` to `new_account/active == true`.

The previous Phase-3 commands operating on `new_account/buffer` are
retired in favor of the new set above.

## 8. View tree projection

The page renderer (`IndexRenderer` in
`crates/ox-cli/src/settings/renderers/index.rs`) currently emits:

```rust
View::Frame {
    title, title_right,
    content: Box::new(View::List { items, selected }),
}
```

When `ui/settings/new_account/active == true`, the renderer emits
instead:

```rust
View::Frame {
    title, title_right,
    content: Box::new(View::Stack {
        dir: Direction::Vertical,
        children: vec![
            (compose_form_view(...), Sizing::Fixed(form_height)),
            (View::List { items, selected }, Sizing::Fill),
        ],
    }),
}
```

`compose_form_view` iterates `FIELD_ORDER`, calling a per-variant
projection that reads that field's state path and builds its
`FormRow`. The enum → index translation happens once, at the
`View::Form.focused` boundary:

```rust
fn compose_form_view(broker: &Broker) -> View {
    let focused_field: AccountField = read_focused_field(broker);
    let errors: ValidationErrors = read_errors(broker);

    let rows: Vec<FormRow> = FIELD_ORDER
        .iter()
        .map(|field| project_field(broker, *field, errors.for_field(*field)))
        .collect();

    let focused_idx = FIELD_ORDER
        .iter()
        .position(|f| *f == focused_field);

    View::Form { rows, focused: focused_idx }
}

fn project_field(
    broker: &Broker,
    field: AccountField,
    error: Option<&str>,
) -> FormRow {
    let label = field_label(field).to_string();
    let error = error.map(String::from);
    let (value, hint) = match field {
        AccountField::Name => (
            text_value(read_string(broker, field), /*masked=*/ false),
            None,
        ),
        AccountField::Protocol => (
            selector_value(read_protocol(broker), PROTOCOL_OPTIONS),
            None,
        ),
        AccountField::Endpoint => (
            text_value(read_string(broker, field), false),
            None,
        ),
        AccountField::Auth => (
            selector_value(
                read_auth(broker).map(|a| a.to_string()),
                AUTH_OPTIONS,
            ),
            None,
        ),
        AccountField::Key => (
            text_value(read_string(broker, field), /*masked=*/ true),
            Some("required for x-api-key / bearer-token".into()),
        ),
    };
    FormRow { label, value, error, hint }
}
```

`text_value(v, masked)` returns
`FormValue::Text { value: v.clone(), cursor: v.chars().count() as u32, masked }`.

`selector_value(current, options)`:
- `None` → `FormValue::ReadOnly("(not selected)".into())`
- `Some(x)` → `FormValue::Selector { options: options.to_vec(), current: <position of x in options> }`

`form_height` is `rows.len() + chrome` (constant computed in the
renderer). No new translator code.

When `new_account/active != true`, the page emits the existing
`View::Frame → View::List` shape unchanged.

## 9. Commit

The commit handler (`accounts.compose.commit`):

1. Read the draft state.
2. Compute validation.
3. If any error is `Some`, no-op (errors already visible via
   `FormRow.error`).
4. Else, build:
   - `display_name = name.trim().to_string()` — the user-typed string, arbitrary Unicode.
   - `path_id = namecode::encode(&display_name)` — opaque valid-XID identifier; for inputs that are already valid XID this is a byte-for-byte no-op.
   - `acct_path = config/gate/accounts/<path_id>`.
   - `provider_path = config/gate/providers/<path_id>` (per-account naming — the bug fix).
   - `key_path = secret/keys/<path_id>` (if `auth.requires_key()`).
   - `AccountConfig { provider: path_id.clone(), display_name: Some(display_name.clone()) }`.
   - `ProviderConfig`:
     - `dialect: protocol.clone()` (the selected option).
     - `endpoint: endpoint.trim().to_string()`.
     - `version: protocol_default_version(protocol)` (e.g., "2023-06-01" for anthropic).
     - `auth: Some(auth.clone())`.
   - `ApiKey(key.trim().to_string())` if auth requires.
5. Writes (one batch):
   - `acct_path ← AccountConfig`.
   - `provider_path ← ProviderConfig`.
   - `key_path ← ApiKey` (conditional).
   - `ui/settings/accounts/selected ← Some(path_id)`.
   - `ui/settings/focused ← settings/accounts/<path_id>`.
   - `ui/settings/expanded ← (existing set ∪ {settings/accounts, settings/accounts/<path_id>})`.
   - `ui/settings/new_account ← Null` (clears all draft state via Phase-8 cascade).

After commit, the page reverts to `Frame → List` (no Form),
focuses + expands the new account row.

## 10. What gets removed/replaced

- `ui/settings/new_account/buffer: Option<String>` — replaced by the
  five-field sub-tree.
- The Phase-3 `accounts.compose.*` commands operating on `buffer` —
  replaced by the new command set listed in §6.
- The single-line `Name▸ <buffer>▏` inline prompt (a hand-rolled
  `ListItem`) — replaced by the `View::Form` block.
- `accounts.add` command — renamed (or its body folded into
  `accounts.compose.open`). The keystroke binding (`a`) stays.

The dispatcher's compose-mode pass and `_compose_new_account`
synthetic scope stay; the keys bound at that scope change, and the
discriminator updates to `active == true`.

## 11. Tests

Unit tests in `account_model.rs`:
- `validate_compose_*` per-field tests (empty/valid/duplicate-name/invalid-name/auth-requires-key/etc.).
- `accounts_compose_insert_char` appends to focused text field + recomputes errors.
- `accounts_compose_insert_char_noop_on_selector` (focused_field = Protocol → no buffer change anywhere).
- `accounts_compose_cycle_forward_picks_first_when_none` (first h/l press on unselected selector).
- `focus_next_walks_field_order` cycles Name → Protocol → Endpoint → Auth → Key → Name.
- `focus_prev_walks_field_order_reversed`.
- `accounts_compose_commit_with_errors` is no-op.
- `accounts_compose_commit_clean_writes_account_provider_key_and_clears_state`.
- `accounts_compose_cancel_clears_draft`.

View-tree tests in `crates/ox-cli/src/settings/renderers/index.rs`:
- When `active == true`, the rendered View is `Frame → Stack → [Form, List]`; assert `View::Form.rows.len() == FIELD_ORDER.len()` and `focused == Some(FIELD_ORDER.iter().position(|f| *f == AccountField::Name))` initially.
- `FormRow[i].label == field_label(FIELD_ORDER[i])` for each i; value variant matches `field_kind(FIELD_ORDER[i])`.
- Errors thread through to `FormRow.error` for the matching variant (set an invalid name, assert `rows[Name's position].error == Some(...)`).
- When `active != true`, the rendered View is `Frame → List` (no Stack, no Form).
- **Field-identity property test:** for each `AccountField` variant, setting `focused_field = <variant>` and projecting produces a `View::Form { focused: Some(i), .. }` where `i == FIELD_ORDER.iter().position(|f| *f == variant).unwrap()`. The test computes the expected idx from `FIELD_ORDER` (never hardcoding `0..5`), so a future edit to the visual order doesn't require test edits — it just changes the position the assertion derives.

E2E tests in `crates/ox-cli/tests/settings_e2e.rs`:
- Replace `add_connection_inline_ghost_row_accepts_typing` with `add_connection_form_accepts_field_by_field_input` — drives Tab through fields, types into each, asserts via insta after each interaction.
- Update `add_account_create_flow` for the new flow shape.
- `add_connections_have_independent_providers` reproducer (commit `b89f3b6`) turns green.

## 12. Risks

- **Stack sizing.** `View::Form` height isn't trivially known
  ahead of time — depends on whether any errors are showing
  (each error consumes a line in `render_form`). Mitigation:
  compute height in the renderer from the projected rows
  (`rows.len() + rows.iter().filter(|r| r.error.is_some()).count()`)
  before constructing the Stack. Validated by snapshot tests.
- **Validation drift.** The `validate_compose_*` functions
  duplicate logic that exists for real-account inline edits
  (PathComponent name validity). Mitigation: PathComponent::try_new
  is the shared primitive; compose's helpers wrap it with
  compose-specific messages.
- **Dispatch shadowing.** Compose-mode's `h`/`l` bindings shadow
  the existing protocol-cycle keystrokes at real-account focus
  when compose is active. The dispatcher's compose-mode pass
  already takes priority (Phase 4 order:
  pending-delete → manual-model → compose → edit → focused-row →
  page cursor), so this is structurally handled — but worth a
  test that confirms a real-account row's `l` doesn't trigger
  compose's cycle while compose is active.
- **Per-account provider naming.** `account_name` is the
  user-typed string; collision-checking against existing accounts
  is part of validation. If a future feature needs a different
  naming scheme (e.g., UUID-based), this is a re-design moment.
  Out of scope.

## 13. Execution

Single implementation phase. Plan organizes as ~3 commits:
1. Add `ValidationErrors` type, FIELDS layout, validation
   functions. Inline unit tests.
2. Add the compose-mode command set + bindings + dispatcher
   discriminator update. Retire the Phase-3 single-field
   commands. E2E test updates.
3. Update `IndexRenderer` to emit
   `Frame → Stack → [Form, List]` when active, projecting state
   into `View::Form`. View-tree assertion tests + insta snapshots.

After landing:
- `add_connections_have_independent_providers` reproducer turns green.
- Existing inline-create E2E gets a new shape (replay rewritten).
- The reified-by-intent `View::Form` shape is exercised by a
  real feature, ready for follow-up adoption by manual-model
  and real-account inline edits.
