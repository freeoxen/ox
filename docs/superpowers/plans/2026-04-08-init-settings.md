# Init + Settings Screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `ox init` wizard, first-run detection, and a Settings screen so users can configure accounts and API keys without hand-editing TOML or passing env vars.

**Architecture:** Key files in `~/.ox/keys/` replace the `key` field on AccountEntry/AccountConfig. A new `resolve_keys()` function loads keys from files and env vars, injecting them into the flat config map. The Settings screen is a new `Screen::Settings` variant in UiStore, rendered by a dedicated `settings_view.rs`, with account CRUD state managed in a local `SettingsState` struct (like `DialogState`). Wizard mode is a guided overlay on the same screen.

**Tech Stack:** Rust, ratatui (TUI rendering), clap (subcommands), figment (config), tokio (async broker), StructFS (Reader/Writer)

**Spec:** `docs/superpowers/specs/2026-04-08-init-settings-design.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/ox-cli/src/config.rs` | Modify: remove `key` from `AccountEntry`, add `endpoint`, add `resolve_keys()`, add `write_account()`, `delete_account()`, `write_key_file()`, `read_key_file()` |
| `crates/ox-gate/src/account.rs` | Modify: remove `key` from `AccountConfig` |
| `crates/ox-gate/src/lib.rs` | Modify: update `GateStore::new()` account construction, snapshot restore |
| `crates/ox-gate/src/tools.rs` | Modify: update test AccountConfig construction |
| `crates/ox-web/src/lib.rs` | Modify: update `set_api_key()` AccountConfig construction |
| `crates/ox-cli/src/main.rs` | Modify: add `ox init` subcommand, first-run detection, replace hard exit |
| `crates/ox-ui/src/ui_store.rs` | Modify: add `Screen::Settings` variant |
| `crates/ox-cli/src/bindings.rs` | Modify: add `s` keybinding for settings |
| `crates/ox-cli/src/settings_view.rs` | Create: settings screen rendering |
| `crates/ox-cli/src/settings_state.rs` | Create: settings local state (accounts list, editing, wizard step) |
| `crates/ox-cli/src/tui.rs` | Modify: route to `settings_view::draw_settings` |
| `crates/ox-cli/src/event_loop.rs` | Modify: handle settings screen keys and pending actions |
| `crates/ox-cli/src/view_state.rs` | Modify: add settings-related fields to ViewState |

---

### Task 1: Remove `key` from AccountConfig and AccountEntry

**Files:**
- Modify: `crates/ox-gate/src/account.rs`
- Modify: `crates/ox-gate/src/lib.rs`
- Modify: `crates/ox-gate/src/tools.rs`
- Modify: `crates/ox-web/src/lib.rs`
- Modify: `crates/ox-cli/src/config.rs`

The `key` field is removed from both the ox-gate `AccountConfig` (runtime type) and the ox-cli `AccountEntry` (figment type). Keys are resolved separately and injected into the flat config map.

- [ ] **Step 1: Remove `key` from `AccountConfig`**

In `crates/ox-gate/src/account.rs`, remove the `key` field:

```rust
//! Account configuration for LLM API access.

use serde::{Deserialize, Serialize};

/// An account binds to a named provider (dialect + optional endpoint override).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Name of the provider dialect (e.g. `"anthropic"`, `"openai"`).
    pub provider: String,
}
```

- [ ] **Step 2: Fix all AccountConfig construction sites in ox-gate**

In `crates/ox-gate/src/lib.rs`, update `GateStore::new()`:
```rust
accounts.insert(
    "anthropic".to_string(),
    AccountConfig {
        provider: "anthropic".to_string(),
    },
);
accounts.insert(
    "openai".to_string(),
    AccountConfig {
        provider: "openai".to_string(),
    },
);
```

Update `restore_from_snapshot()` — remove `key: String::new()`:
```rust
new_accounts.insert(
    name.clone(),
    AccountConfig {
        provider,
    },
);
```

In `crates/ox-gate/src/tools.rs`, update test AccountConfig construction (remove `key` field). Tests that construct `AccountConfig { provider: "test".to_string(), key: "sk-test".to_string() }` become `AccountConfig { provider: "test".to_string() }`.

In `crates/ox-web/src/lib.rs`, update `set_api_key()` — the AccountConfig no longer has a key field. The key is written separately to the gate store path:
```rust
let config = AccountConfig {
    provider: provider.to_string(),
};
```

- [ ] **Step 3: Remove `key` from `AccountEntry`, add `endpoint`**

In `crates/ox-cli/src/config.rs`, update `AccountEntry`:
```rust
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountEntry {
    pub provider: String,
    #[serde(default)]
    pub endpoint: Option<String>,
}
```

Update the module doc comment:
```rust
//! Config resolution via figment — defaults → TOML file → env vars → CLI flags.
//! Config shape: gate.accounts.{name}.{provider,endpoint} + gate.defaults.{account,model,max_tokens}
```

Update `to_flat_map()` — remove the key insertion, add endpoint:
```rust
pub fn to_flat_map(&self) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    for (name, entry) in &self.gate.accounts {
        map.insert(
            format!("gate/accounts/{name}/provider"),
            Value::String(entry.provider.clone()),
        );
        if let Some(ref ep) = entry.endpoint {
            map.insert(
                format!("gate/accounts/{name}/endpoint"),
                Value::String(ep.clone()),
            );
        }
    }
    map.insert(
        "gate/defaults/account".into(),
        Value::String(self.gate.defaults.account.clone()),
    );
    map.insert(
        "gate/defaults/model".into(),
        Value::String(self.gate.defaults.model.clone()),
    );
    map.insert(
        "gate/defaults/max_tokens".into(),
        Value::Integer(self.gate.defaults.max_tokens),
    );
    map
}
```

- [ ] **Step 4: Fix GateStore Reader/Writer for key removal**

In `crates/ox-gate/src/lib.rs`, the `accounts/{name}/key` path in the Reader now ONLY reads from config handle (no local key on AccountConfig). The Writer for `accounts/{name}/key` should be removed since keys are not stored on AccountConfig.

In the Reader `accounts` arm, remove the local key check for the account. The config handle check already handles it:
```rust
"accounts" => {
    if from.components.len() < 2 {
        return Ok(None);
    }
    let name = from.components[1].as_str();

    // Check config for per-account key
    if from.components.len() > 2 {
        let field = from.components[2].as_str();
        if field == "key" {
            if let Some(k) =
                self.config_string(&format!("gate/accounts/{name}/key"))
            {
                return Ok(Some(Record::parsed(Value::String(k))));
            }
            // No local key on AccountConfig anymore
            return Ok(Some(Record::parsed(Value::String(String::new()))));
        }
    }

    let Some(config) = self.accounts.get(name) else {
        return Ok(None);
    };

    if from.components.len() == 2 {
        let value = to_value(config)
            .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
        return Ok(Some(Record::parsed(value)));
    }

    let field = from.components[2].as_str();
    match field {
        "provider" => Ok(Some(Record::parsed(Value::String(
            config.provider.clone(),
        )))),
        _ => Ok(None),
    }
}
```

In the Writer `accounts` arm, remove the `"key"` match arm entirely. Keys are written to key files, not to GateStore.

- [ ] **Step 5: Update tests**

Update all tests in `crates/ox-gate/src/lib.rs` that write to `accounts/{name}/key` — they should now set keys via the config handle (LocalConfig) instead:

For `test_tools_schemas_with_keys`:
```rust
#[test]
fn test_tools_schemas_with_keys() {
    use ox_store_util::LocalConfig;
    let mut config = LocalConfig::new();
    config.set(
        "gate/accounts/anthropic/key",
        Value::String("sk-test".into()),
    );
    let mut gate = GateStore::new().with_config(Box::new(config));
    let record = gate.read(&path!("tools/schemas")).unwrap().unwrap();
    let json = match record {
        Record::Parsed(v) => value_to_json(v),
        _ => panic!("expected parsed"),
    };
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "complete_anthropic");
}
```

Apply the same pattern to `test_create_completion_tools`, `test_account_key_roundtrip`, `snapshot_excludes_api_keys`.

Update config.rs tests — remove `key` from TOML test fixtures and assertions:
```toml
[gate.accounts.personal]
provider = "anthropic"

[gate.accounts.openai]
provider = "openai"
```

Remove assertions about `config.gate.accounts["personal"].key`.

- [ ] **Step 6: Run tests and check**

Run: `cargo test -p ox-gate && cargo test -p ox-cli -- config && cargo check --workspace`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-gate/ crates/ox-cli/src/config.rs crates/ox-web/src/lib.rs
git commit -m "refactor: remove key field from AccountConfig and AccountEntry

Keys are resolved from key files and env vars, not deserialized from config.
AccountEntry gains optional endpoint field for custom API URLs."
```

---

### Task 2: Key file resolution

**Files:**
- Modify: `crates/ox-cli/src/config.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Add `resolve_keys()` and key file helpers**

In `crates/ox-cli/src/config.rs`, add after `resolve_config`:

```rust
use std::path::Path;

/// Resolve API keys from key files and env vars.
///
/// For each account in config, checks:
/// 1. Env var `OX_GATE__ACCOUNTS__{NAME}__KEY` (highest priority)
/// 2. Key file `{keys_dir}/{name}.key`
///
/// Returns a map of account name → API key.
pub fn resolve_keys(
    keys_dir: &Path,
    config: &OxConfig,
) -> BTreeMap<String, String> {
    let mut keys = BTreeMap::new();
    for name in config.gate.accounts.keys() {
        let env_var = format!(
            "OX_GATE__ACCOUNTS__{}__KEY",
            name.to_uppercase()
        );
        if let Ok(k) = std::env::var(&env_var) {
            if !k.is_empty() {
                keys.insert(name.clone(), k);
                continue;
            }
        }
        if let Ok(contents) = std::fs::read_to_string(keys_dir.join(format!("{name}.key"))) {
            let trimmed = contents.trim().to_string();
            if !trimmed.is_empty() {
                keys.insert(name.clone(), trimmed);
            }
        }
    }
    keys
}

/// Write an API key to a key file, creating the keys directory if needed.
pub fn write_key_file(keys_dir: &Path, name: &str, key: &str) -> std::io::Result<()> {
    if !keys_dir.exists() {
        std::fs::create_dir_all(keys_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(keys_dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    std::fs::write(keys_dir.join(format!("{name}.key")), key)
}

/// Read an API key from a key file.
pub fn read_key_file(keys_dir: &Path, name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(keys_dir.join(format!("{name}.key"))).ok()?;
    let trimmed = contents.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

/// Delete a key file.
pub fn delete_key_file(keys_dir: &Path, name: &str) -> std::io::Result<()> {
    let path = keys_dir.join(format!("{name}.key"));
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Check if any account has a usable key (from key files or env vars).
pub fn has_any_key(keys_dir: &Path, config: &OxConfig) -> bool {
    !resolve_keys(keys_dir, config).is_empty()
}
```

- [ ] **Step 2: Update `to_flat_map` to accept resolved keys**

Add a method that merges resolved keys into the flat map:

```rust
impl OxConfig {
    /// Produce the flat config map with resolved keys injected.
    pub fn to_flat_map_with_keys(
        &self,
        keys: &BTreeMap<String, String>,
    ) -> BTreeMap<String, Value> {
        let mut map = self.to_flat_map();
        for (name, key) in keys {
            map.insert(
                format!("gate/accounts/{name}/key"),
                Value::String(key.clone()),
            );
        }
        map
    }
}
```

- [ ] **Step 3: Update main.rs to use key file resolution**

Replace the hard exit with key resolution:

```rust
    let resolved = config::resolve_config(&inbox_root, &overrides);
    let keys_dir = inbox_root.join("keys");
    let resolved_keys = config::resolve_keys(&keys_dir, &resolved);
    let needs_setup = resolved_keys.is_empty();

    let flat_config = resolved.to_flat_map_with_keys(&resolved_keys);
```

Remove the hard `exit(1)` block (lines 75-91). The `needs_setup` flag will be used in Task 7 for wizard mode; for now, just let it proceed (the agent will fail gracefully if no key is present).

- [ ] **Step 4: Write tests for key resolution**

In `crates/ox-cli/src/config.rs` tests:

```rust
#[test]
fn resolve_keys_from_files() {
    let dir = tempfile::tempdir().unwrap();
    let keys_dir = dir.path().join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    std::fs::write(keys_dir.join("anthropic.key"), "sk-test-key\n").unwrap();

    let mut config = OxConfig::default();
    config.gate.accounts.insert(
        "anthropic".into(),
        AccountEntry {
            provider: "anthropic".into(),
            endpoint: None,
        },
    );

    let keys = resolve_keys(&keys_dir, &config);
    assert_eq!(keys.get("anthropic").unwrap(), "sk-test-key");
}

#[test]
fn resolve_keys_env_beats_file() {
    let dir = tempfile::tempdir().unwrap();
    let keys_dir = dir.path().join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    std::fs::write(keys_dir.join("testacct.key"), "from-file").unwrap();

    let mut config = OxConfig::default();
    config.gate.accounts.insert(
        "testacct".into(),
        AccountEntry {
            provider: "anthropic".into(),
            endpoint: None,
        },
    );

    unsafe { std::env::set_var("OX_GATE__ACCOUNTS__TESTACCT__KEY", "from-env"); }
    let keys = resolve_keys(&keys_dir, &config);
    assert_eq!(keys.get("testacct").unwrap(), "from-env");
    unsafe { std::env::remove_var("OX_GATE__ACCOUNTS__TESTACCT__KEY"); }
}

#[test]
fn write_and_read_key_file() {
    let dir = tempfile::tempdir().unwrap();
    let keys_dir = dir.path().join("keys");
    write_key_file(&keys_dir, "test", "sk-12345").unwrap();
    assert_eq!(read_key_file(&keys_dir, "test").unwrap(), "sk-12345");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&keys_dir).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o700);
    }
}

#[test]
fn has_any_key_false_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let config = OxConfig::default();
    assert!(!has_any_key(&dir.path().join("keys"), &config));
}

#[test]
fn to_flat_map_with_keys_injects_keys() {
    let mut config = OxConfig::default();
    config.gate.accounts.insert(
        "anthropic".into(),
        AccountEntry {
            provider: "anthropic".into(),
            endpoint: None,
        },
    );
    let mut keys = BTreeMap::new();
    keys.insert("anthropic".into(), "sk-injected".into());
    let flat = config.to_flat_map_with_keys(&keys);
    assert_eq!(
        flat.get("gate/accounts/anthropic/key").unwrap(),
        &Value::String("sk-injected".into())
    );
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-cli -- config`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/config.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): key file resolution from ~/.ox/keys/

resolve_keys() loads API keys from key files and env vars.
Keys directory created with 0700 permissions.
Removes hard exit(1) on missing key — setup wizard will handle it."
```

---

### Task 3: `Screen::Settings` and basic navigation

**Files:**
- Modify: `crates/ox-ui/src/ui_store.rs`
- Modify: `crates/ox-cli/src/bindings.rs`
- Create: `crates/ox-cli/src/settings_state.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/tui.rs`
- Modify: `crates/ox-cli/src/view_state.rs`
- Create: `crates/ox-cli/src/settings_view.rs`
- Modify: `crates/ox-cli/src/main.rs`

This task adds the Settings screen shell — navigation in/out, empty rendering, state struct. No account CRUD yet.

- [ ] **Step 1: Add `Screen::Settings` to UiStore**

In `crates/ox-ui/src/ui_store.rs`, add the variant:

```rust
pub enum Screen {
    Inbox,
    Thread,
    Settings,
}
```

Add the `"settings"` command handler in the Writer impl. Search for the `"open"` command handling and add a new command:

In the UiStore Writer, add a `"go_to_settings"` command (alongside `"go_to_inbox"`, `"open"`, etc.):
```rust
"go_to_settings" => {
    self.screen = Screen::Settings;
    self.mode = Mode::Normal;
    Ok(to.clone())
}
```

In the Reader, update the `"screen"` read to return `"settings"` for `Screen::Settings`:
```rust
Screen::Settings => "settings",
```

- [ ] **Step 2: Add `s` keybinding for settings**

In `crates/ox-cli/src/bindings.rs`, in `normal_mode()`, add:

```rust
out.push(bind_screen(
    "normal",
    "s",
    "inbox",
    cmd("ui/go_to_settings"),
    "Open settings",
));
```

Add `Esc` and `q` to exit settings back to inbox:
```rust
out.push(bind_screen(
    "normal",
    "Esc",
    "settings",
    cmd("ui/go_to_inbox"),
    "Back to inbox",
));
out.push(bind_screen(
    "normal",
    "q",
    "settings",
    cmd("ui/go_to_inbox"),
    "Back to inbox",
));
```

- [ ] **Step 3: Create `settings_state.rs`**

Create `crates/ox-cli/src/settings_state.rs`:

```rust
//! Local state for the Settings screen.
//!
//! Owned by the event loop, not stored in the broker (ephemeral UI state).

/// Which section of settings has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFocus {
    Accounts,
    Defaults,
}

/// Wizard step for guided setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    AddAccount,
    SetDefaults,
    Done,
}

/// Fields for the account add/edit dialog.
#[derive(Debug, Clone, Default)]
pub struct AccountEditFields {
    pub name: String,
    pub dialect: usize,        // 0=anthropic, 1=openai
    pub endpoint: String,
    pub key: String,
    pub focus: usize,          // which field has cursor: 0=name, 1=dialect, 2=endpoint, 3=key
    pub is_new: bool,          // true=add, false=edit
}

pub const DIALECTS: [&str; 2] = ["anthropic", "openai"];

/// Test connection status.
#[derive(Debug, Clone)]
pub enum TestStatus {
    Idle,
    Testing,
    Success(String),   // e.g. "Connected (anthropic, 200ms)"
    Failed(String),    // e.g. "invalid API key"
}

/// Account summary for display.
#[derive(Debug, Clone)]
pub struct AccountSummary {
    pub name: String,
    pub dialect: String,
    pub endpoint_display: String,  // hostname or "default"
    pub has_key: bool,
    pub is_default: bool,
}

/// Settings screen local state.
pub struct SettingsState {
    pub focus: SettingsFocus,
    pub selected_account: usize,
    pub accounts: Vec<AccountSummary>,
    pub editing: Option<AccountEditFields>,
    pub test_status: TestStatus,
    pub wizard: Option<WizardStep>,
    /// Defaults
    pub default_account_idx: usize,
    pub default_model_idx: usize,
    pub default_max_tokens: String,
    pub defaults_focus: usize,  // 0=account, 1=model, 2=max_tokens
}

impl SettingsState {
    pub fn new() -> Self {
        Self {
            focus: SettingsFocus::Accounts,
            selected_account: 0,
            accounts: Vec::new(),
            editing: None,
            test_status: TestStatus::Idle,
            wizard: None,
            default_account_idx: 0,
            default_model_idx: 0,
            default_max_tokens: "4096".to_string(),
            defaults_focus: 0,
        }
    }

    pub fn new_wizard() -> Self {
        let mut s = Self::new();
        s.wizard = Some(WizardStep::AddAccount);
        s.editing = Some(AccountEditFields {
            name: String::new(),
            dialect: 0,
            endpoint: String::new(),
            key: String::new(),
            focus: 0,
            is_new: true,
        });
        s
    }
}
```

- [ ] **Step 4: Create `settings_view.rs` (stub)**

Create `crates/ox-cli/src/settings_view.rs`:

```rust
//! Settings screen rendering.

use crate::settings_state::SettingsState;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Draw the settings screen content area.
pub(crate) fn draw_settings(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let title = if state.wizard.is_some() {
        " Setup Wizard "
    } else {
        " Settings "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.border);

    let placeholder = Paragraph::new("Settings screen — account management coming next")
        .block(block);
    frame.render_widget(placeholder, area);
}
```

- [ ] **Step 5: Wire into tui.rs and event_loop.rs**

In `crates/ox-cli/src/main.rs`, add module declarations:
```rust
mod settings_state;
mod settings_view;
```

In `crates/ox-cli/src/tui.rs`, update the `draw` function to route to settings:

After the content area section (around line 53), add a settings branch:
```rust
    if vs.screen == "settings" {
        crate::settings_view::draw_settings(
            frame,
            settings_state,
            theme,
            content_area,
        );
    } else if vs.active_thread.is_some() {
```

This requires `draw()` to accept `&SettingsState`. Update the signature:
```rust
pub(crate) fn draw(
    frame: &mut Frame,
    vs: &ViewState,
    settings_state: &crate::settings_state::SettingsState,
    theme: &Theme,
) -> (Option<usize>, usize) {
```

In `crates/ox-cli/src/event_loop.rs`, add `SettingsState` to `DialogState` or alongside it:

```rust
use crate::settings_state::SettingsState;
```

In `run_async`, create the settings state:
```rust
    let mut settings = SettingsState::new();
```

Pass it to `draw()`:
```rust
    crate::tui::draw(frame, &vs, &settings, theme);
```

- [ ] **Step 6: Add `screen` field to ViewState if not already present**

The ViewState already has a `screen: String` field. Verify it's populated from UiStore. The settings_view draw function checks `vs.screen == "settings"`.

- [ ] **Step 7: Update status bar hints for settings**

In `crates/ox-cli/src/tui.rs`, in `draw_status_bar`, add a settings case:

```rust
    let hints = match (
        vs.mode.as_str(),
        vs.insert_context.as_deref(),
        vs.active_thread.is_some(),
        vs.screen.as_str(),
    ) {
        (_, _, _, "settings") => " | a add | e edit | d delete | t test | Esc back",
        ("normal", _, false, _) => " | i compose | / search | s settings | Enter open | d archive | q quit",
        ("normal", _, true, _) => " | i reply | j/k scroll | q/Esc inbox",
        ("insert", Some("search"), _, _) => " | Enter chip | Esc cancel",
        ("insert", _, _, _) => " | ^Enter send | Esc cancel",
        _ => "",
    };
```

Note: also add `s settings` hint to the inbox normal mode line.

- [ ] **Step 8: Run and verify**

Run: `cargo check -p ox-cli`
Expected: compiles. Then `cargo test -p ox-cli` — existing tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/ox-ui/src/ui_store.rs crates/ox-cli/src/
git commit -m "feat(ox-cli): settings screen shell with navigation

Screen::Settings variant, s from inbox, Esc/q back.
SettingsState struct for local UI state.
Placeholder settings_view rendering."
```

---

### Task 4: Settings screen — account list rendering

**Files:**
- Modify: `crates/ox-cli/src/settings_view.rs`
- Modify: `crates/ox-cli/src/settings_state.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`

This task fills in the settings screen: reads accounts from config, renders the list, handles j/k navigation within the accounts section.

- [ ] **Step 1: Populate accounts from config on settings entry**

In `crates/ox-cli/src/event_loop.rs`, when the pending action is to enter settings (or when screen transitions to settings), populate `SettingsState.accounts` from the broker's ConfigStore. Add a helper:

```rust
fn refresh_settings_accounts(
    settings: &mut SettingsState,
    config: &config::OxConfig,
    keys_dir: &std::path::Path,
) {
    use crate::settings_state::AccountSummary;
    let keys = config::resolve_keys(keys_dir, config);
    let default_account = &config.gate.defaults.account;

    settings.accounts = config
        .gate
        .accounts
        .iter()
        .map(|(name, entry)| {
            let endpoint_display = entry
                .endpoint
                .as_ref()
                .and_then(|ep| url::Url::parse(ep).ok())
                .map(|u| u.host_str().unwrap_or("custom").to_string())
                .unwrap_or_else(|| match entry.provider.as_str() {
                    "anthropic" => "api.anthropic.com".to_string(),
                    "openai" => "api.openai.com".to_string(),
                    _ => "default".to_string(),
                });
            AccountSummary {
                name: name.clone(),
                dialect: entry.provider.clone(),
                endpoint_display,
                has_key: keys.contains_key(name),
                is_default: name == default_account,
            }
        })
        .collect();
    settings.accounts.sort_by(|a, b| a.name.cmp(&b.name));

    // Update defaults indices
    settings.default_account_idx = settings
        .accounts
        .iter()
        .position(|a| a.is_default)
        .unwrap_or(0);
    settings.default_model_idx = 0; // TODO: match from config in Task 6
    settings.default_max_tokens = config.gate.defaults.max_tokens.to_string();
}
```

Note: `url` crate may not be a dependency. If not, use a simpler hostname extraction: split on `://`, take host part, split on `/`. Or just display the full endpoint string trimmed. Use whatever approach doesn't add a new dependency.

- [ ] **Step 2: Render account list**

Replace the placeholder in `crates/ox-cli/src/settings_view.rs` with a full rendering:

```rust
use crate::settings_state::{SettingsFocus, SettingsState, DIALECTS};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

pub(crate) fn draw_settings(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // title
            Constraint::Min(5),     // accounts
            Constraint::Length(5),  // defaults
            Constraint::Length(1),  // help
        ])
        .split(area);

    // Title
    let title = if state.wizard.is_some() {
        " Setup Wizard "
    } else {
        " Settings "
    };
    frame.render_widget(
        Paragraph::new(Span::styled(title, theme.title_badge)),
        chunks[0],
    );

    // Accounts section
    draw_accounts_section(frame, state, theme, chunks[1]);

    // Defaults section
    draw_defaults_section(frame, state, theme, chunks[2]);

    // Help line
    let help = if state.editing.is_some() {
        " Tab next | Enter save | Esc cancel"
    } else {
        " a add | e edit | d delete | t test | Tab defaults | Esc back"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(help, theme.status)),
        chunks[3],
    );

    // Account edit overlay
    if let Some(ref editing) = state.editing {
        draw_account_edit_dialog(frame, editing, state, theme);
    }
}

fn draw_accounts_section(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let block = Block::default()
        .title(" Accounts ")
        .borders(Borders::ALL)
        .border_style(if state.focus == SettingsFocus::Accounts {
            theme.active_border
        } else {
            theme.border
        });

    let items: Vec<ListItem> = state
        .accounts
        .iter()
        .enumerate()
        .map(|(i, acct)| {
            let marker = if acct.is_default { "●" } else { " " };
            let key_status = if acct.has_key { "✓" } else { "✗" };
            let selected = i == state.selected_account
                && state.focus == SettingsFocus::Accounts;
            let style = if selected {
                theme.selected
            } else {
                theme.normal
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::styled(
                    format!("{:<16}", acct.name),
                    style,
                ),
                Span::styled(
                    format!("{:<12}", acct.dialect),
                    style,
                ),
                Span::styled(
                    format!("{:<24}", acct.endpoint_display),
                    style,
                ),
                Span::styled(format!(" {key_status}"), style),
            ]))
        })
        .collect();

    if items.is_empty() {
        let empty = Paragraph::new("  No accounts configured. Press 'a' to add one.")
            .block(block);
        frame.render_widget(empty, area);
    } else {
        let list = List::new(items).block(block);
        frame.render_widget(list, area);
    }
}

fn draw_defaults_section(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let block = Block::default()
        .title(" Defaults ")
        .borders(Borders::ALL)
        .border_style(if state.focus == SettingsFocus::Defaults {
            theme.active_border
        } else {
            theme.border
        });

    let acct_name = state
        .accounts
        .get(state.default_account_idx)
        .map(|a| a.name.as_str())
        .unwrap_or("(none)");

    let lines = vec![
        Line::from(format!("  Account:    {acct_name}")),
        Line::from(format!("  Model:      (select in defaults editing)")),
        Line::from(format!("  Max tokens: {}", state.default_max_tokens)),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_account_edit_dialog(
    frame: &mut Frame,
    editing: &crate::settings_state::AccountEditFields,
    state: &SettingsState,
    theme: &Theme,
) {
    let area = centered_rect(50, 12, frame.area());
    frame.render_widget(Clear, area);

    let title = if editing.is_new {
        " Add Account "
    } else {
        " Edit Account "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.active_border);

    let dialect_str = DIALECTS.get(editing.dialect).unwrap_or(&"anthropic");
    let key_display = if editing.key.is_empty() {
        "(empty)".to_string()
    } else {
        let len = editing.key.len();
        if len > 4 {
            format!("{}...{}", "●".repeat(len.min(10) - 4), &editing.key[len - 4..])
        } else {
            "●".repeat(len)
        }
    };

    let test_line = match &state.test_status {
        crate::settings_state::TestStatus::Idle => String::new(),
        crate::settings_state::TestStatus::Testing => "  Testing...".to_string(),
        crate::settings_state::TestStatus::Success(msg) => format!("  ✓ {msg}"),
        crate::settings_state::TestStatus::Failed(msg) => format!("  ✗ {msg}"),
    };

    let focus = editing.focus;
    let lines = vec![
        Line::from(format!(
            "  Name:     {}{}",
            if focus == 0 { "▸ " } else { "  " },
            editing.name
        )),
        Line::from(format!(
            "  Dialect:  {}{}",
            if focus == 1 { "▸ " } else { "  " },
            dialect_str
        )),
        Line::from(format!(
            "  Endpoint: {}{}",
            if focus == 2 { "▸ " } else { "  " },
            if editing.endpoint.is_empty() {
                "(default)".to_string()
            } else {
                editing.endpoint.clone()
            }
        )),
        Line::from(format!(
            "  API Key:  {}{}",
            if focus == 3 { "▸ " } else { "  " },
            key_display
        )),
        Line::from(""),
        Line::from(format!(
            "  [t]est  [Tab] next field  [Enter] save  [Esc] cancel{test_line}"
        )),
    ];

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Create a centered rectangle.
fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let x = (r.width.saturating_sub(popup_width)) / 2;
    let y = (r.height.saturating_sub(height)) / 2;
    Rect::new(
        r.x + x,
        r.y + y,
        popup_width.min(r.width),
        height.min(r.height),
    )
}
```

- [ ] **Step 3: Handle j/k navigation in settings**

In `crates/ox-cli/src/event_loop.rs`, in the key dispatch section, add settings-screen-specific handling. When screen is "settings" and no edit dialog is open, handle:
- `j`/`Down` — next account
- `k`/`Up` — previous account
- `Tab` — toggle focus between Accounts and Defaults
- `a` — open add account dialog
- `e` — open edit account dialog (populate from selected)
- `d` — delete selected account
- `t` — test connection for selected account

For now, implement `j`/`k`/`Tab` navigation. The others will be wired in Tasks 5-6.

Add to the event_loop key handling (after the broker dispatch, in the settings screen branch):

```rust
if screen_owned == "settings" && settings.editing.is_none() {
    match key_str.as_str() {
        "j" | "Down" => {
            if settings.focus == SettingsFocus::Accounts
                && !settings.accounts.is_empty()
            {
                settings.selected_account =
                    (settings.selected_account + 1).min(settings.accounts.len() - 1);
            }
        }
        "k" | "Up" => {
            if settings.focus == SettingsFocus::Accounts {
                settings.selected_account =
                    settings.selected_account.saturating_sub(1);
            }
        }
        "Tab" => {
            settings.focus = match settings.focus {
                SettingsFocus::Accounts => SettingsFocus::Defaults,
                SettingsFocus::Defaults => SettingsFocus::Accounts,
            };
        }
        _ => {}
    }
}
```

- [ ] **Step 4: Run and verify**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles, tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "feat(ox-cli): settings screen account list rendering

Accounts section with name/dialect/endpoint/key-status display.
Defaults section placeholder. j/k/Tab navigation."
```

---

### Task 5: Account add/edit/delete

**Files:**
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/settings_state.rs`
- Modify: `crates/ox-cli/src/config.rs`

This task wires up the account CRUD: add, edit, delete accounts with key file persistence.

- [ ] **Step 1: Add config write helpers**

In `crates/ox-cli/src/config.rs`, add:

```rust
/// Write an account entry to config.toml.
pub fn write_account(
    config_dir: &Path,
    name: &str,
    entry: &AccountEntry,
) -> std::io::Result<()> {
    let toml_path = config_dir.join("config.toml");
    let mut config = if toml_path.exists() {
        let content = std::fs::read_to_string(&toml_path)?;
        toml::from_str::<OxConfig>(&content).unwrap_or_default()
    } else {
        OxConfig::default()
    };
    config.gate.accounts.insert(name.to_string(), entry.clone());
    let content = toml::to_string_pretty(&config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&toml_path, content)
}

/// Delete an account from config.toml.
pub fn delete_account(config_dir: &Path, name: &str) -> std::io::Result<()> {
    let toml_path = config_dir.join("config.toml");
    if !toml_path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&toml_path)?;
    let mut config: OxConfig = toml::from_str(&content).unwrap_or_default();
    config.gate.accounts.remove(name);
    if config.gate.defaults.account == name {
        config.gate.defaults.account = config
            .gate
            .accounts
            .keys()
            .next()
            .cloned()
            .unwrap_or_default();
    }
    let content = toml::to_string_pretty(&config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&toml_path, content)
}
```

- [ ] **Step 2: Handle `a`, `e`, `d` keys in event loop**

In `crates/ox-cli/src/event_loop.rs`, extend the settings key handling:

```rust
"a" => {
    settings.editing = Some(AccountEditFields {
        name: String::new(),
        dialect: 0,
        endpoint: String::new(),
        key: String::new(),
        focus: 0,
        is_new: true,
    });
    settings.test_status = TestStatus::Idle;
}
"e" => {
    if let Some(acct) = settings.accounts.get(settings.selected_account) {
        let dialect_idx = DIALECTS
            .iter()
            .position(|d| *d == acct.dialect)
            .unwrap_or(0);
        let endpoint = /* read from config */ String::new(); // populated from config
        let key = config::read_key_file(&keys_dir, &acct.name)
            .unwrap_or_default();
        settings.editing = Some(AccountEditFields {
            name: acct.name.clone(),
            dialect: dialect_idx,
            endpoint,
            key,
            focus: 0,
            is_new: false,
        });
        settings.test_status = TestStatus::Idle;
    }
}
"d" => {
    if let Some(acct) = settings.accounts.get(settings.selected_account) {
        let name = acct.name.clone();
        config::delete_account(&inbox_root, &name).ok();
        config::delete_key_file(&keys_dir, &name).ok();
        refresh_settings_accounts(&mut settings, /* re-resolve config */);
    }
}
```

- [ ] **Step 3: Handle edit dialog keys**

When `settings.editing.is_some()`, handle:
- `Tab` — next field (cycle focus 0→1→2→3→0)
- `Esc` — cancel editing
- `Enter` — save account
- `Left`/`Right` on dialect field — toggle between anthropic/openai
- Character input on name/endpoint/key fields — insert character
- `Backspace` on text fields — delete character

```rust
if let Some(ref mut editing) = settings.editing {
    match key_str.as_str() {
        "Tab" => {
            editing.focus = (editing.focus + 1) % 4;
        }
        "Esc" => {
            settings.editing = None;
            settings.test_status = TestStatus::Idle;
        }
        "Enter" => {
            // Save account
            if !editing.name.is_empty() {
                let entry = AccountEntry {
                    provider: DIALECTS[editing.dialect].to_string(),
                    endpoint: if editing.endpoint.is_empty() {
                        None
                    } else {
                        Some(editing.endpoint.clone())
                    },
                };
                config::write_account(&inbox_root, &editing.name, &entry).ok();
                if !editing.key.is_empty() {
                    config::write_key_file(&keys_dir, &editing.name, &editing.key).ok();
                }
                settings.editing = None;
                settings.test_status = TestStatus::Idle;
                refresh_settings_accounts(&mut settings, /* re-resolve */);
            }
        }
        "Left" | "Right" if editing.focus == 1 => {
            editing.dialect = 1 - editing.dialect;
        }
        "Backspace" => {
            match editing.focus {
                0 => { editing.name.pop(); }
                2 => { editing.endpoint.pop(); }
                3 => { editing.key.pop(); }
                _ => {}
            }
        }
        key if key.len() == 1 => {
            let ch = key.chars().next().unwrap();
            match editing.focus {
                0 => editing.name.push(ch),
                2 => editing.endpoint.push(ch),
                3 => editing.key.push(ch),
                _ => {}
            }
        }
        _ => {}
    }
}
```

- [ ] **Step 4: Run and verify**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles, tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "feat(ox-cli): account add/edit/delete in settings

Config write helpers persist accounts to config.toml.
Key files written to ~/.ox/keys/ with 0700 permissions.
Edit dialog with Tab/Enter/Esc/character input."
```

---

### Task 6: Test connection

**Files:**
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/settings_state.rs`

- [ ] **Step 1: Implement test connection**

In `crates/ox-cli/src/event_loop.rs`, handle `t` key in settings (both in account list and edit dialog):

```rust
"t" => {
    let (api_key, dialect, endpoint) = if let Some(ref editing) = settings.editing {
        (
            editing.key.clone(),
            DIALECTS[editing.dialect].to_string(),
            if editing.endpoint.is_empty() {
                None
            } else {
                Some(editing.endpoint.clone())
            },
        )
    } else if let Some(acct) = settings.accounts.get(settings.selected_account) {
        let key = config::read_key_file(&keys_dir, &acct.name)
            .unwrap_or_default();
        (key, acct.dialect.clone(), None /* read from config */)
    } else {
        continue; // or return
    };

    if api_key.is_empty() {
        settings.test_status = TestStatus::Failed("No API key".into());
    } else {
        settings.test_status = TestStatus::Testing;
        // Build provider config
        let provider_config = match dialect.as_str() {
            "openai" => {
                let mut pc = ox_gate::ProviderConfig::openai();
                if let Some(ep) = endpoint {
                    pc.endpoint = ep;
                }
                pc
            }
            _ => {
                let mut pc = ox_gate::ProviderConfig::anthropic();
                if let Some(ep) = endpoint {
                    pc.endpoint = ep;
                }
                pc
            }
        };

        // Minimal completion request
        let request = ox_kernel::CompletionRequest {
            model: match dialect.as_str() {
                "openai" => "gpt-4o-mini".to_string(),
                _ => "claude-haiku-4-5-20251001".to_string(),
            },
            max_tokens: 1,
            system: String::new(),
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
            tools: vec![],
            stream: true,
        };

        let start = std::time::Instant::now();
        let send = crate::transport::make_send_fn(provider_config.clone(), api_key);
        match send(&request) {
            Ok(_) => {
                let elapsed = start.elapsed().as_millis();
                settings.test_status = TestStatus::Success(
                    format!("Connected ({dialect}, {elapsed}ms)")
                );
            }
            Err(e) => {
                settings.test_status = TestStatus::Failed(e);
            }
        }
    }
}
```

Note: `make_send_fn` returns a blocking function. Since the event loop is async, this test call should be spawned on a blocking thread with `tokio::task::spawn_blocking`. The result is polled on the next frame. For simplicity in the first pass, a synchronous call on the event loop thread is acceptable (the TUI freezes briefly during the test) — async test connection can be a follow-up.

- [ ] **Step 2: Run and verify**

Run: `cargo check -p ox-cli`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "feat(ox-cli): test connection in settings

Sends minimal completion to verify API key works.
Shows spinner → success/failure in edit dialog."
```

---

### Task 7: Wizard mode and `ox init`

**Files:**
- Modify: `crates/ox-cli/src/main.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/settings_state.rs`

- [ ] **Step 1: Add `ox init` subcommand**

In `crates/ox-cli/src/main.rs`, add clap subcommand:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ox", about = "Agentic coding CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long)]
    account: Option<String>,

    #[arg(long, short)]
    model: Option<String>,

    #[arg(long, default_value = ".")]
    workspace: String,

    #[arg(long)]
    max_tokens: Option<u32>,

    #[arg(long)]
    no_policy: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive setup wizard
    Init,
}
```

- [ ] **Step 2: Wire startup mode flag**

In `main()`, after config resolution:

```rust
    let force_wizard = matches!(cli.command, Some(Commands::Init));
    let needs_setup = force_wizard || !config::has_any_key(&keys_dir, &resolved);
```

Pass `needs_setup` to the event loop (add it as a parameter to `run_async` or pass it through the app):

```rust
    let result = rt.block_on(event_loop::run_async(
        &mut app,
        &client,
        &theme,
        &mut terminal,
        needs_setup,
    ));
```

- [ ] **Step 3: Enter wizard mode on startup**

In `crates/ox-cli/src/event_loop.rs`, update `run_async` to accept `needs_setup: bool`:

```rust
pub async fn run_async(
    app: &mut App,
    client: &ox_broker::ClientHandle,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
    needs_setup: bool,
) -> std::io::Result<()> {
```

At the start of the function, if `needs_setup`:

```rust
    let mut settings = if needs_setup {
        // Navigate to settings screen
        client
            .write(
                &path!("ui/go_to_settings"),
                structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
            )
            .await
            .ok();
        SettingsState::new_wizard()
    } else {
        SettingsState::new()
    };
```

- [ ] **Step 4: Handle wizard step transitions**

In the event loop, after account save in the edit dialog (Enter key), if wizard mode is active:

```rust
if let Some(ref mut step) = settings.wizard {
    match step {
        WizardStep::AddAccount => {
            // Account was just saved, move to defaults
            *step = WizardStep::SetDefaults;
            settings.focus = SettingsFocus::Defaults;
        }
        WizardStep::SetDefaults => {
            // Defaults confirmed, wizard complete
            *step = WizardStep::Done;
        }
        WizardStep::Done => {
            // Transition to inbox
            settings.wizard = None;
            client
                .write(
                    &path!("ui/go_to_inbox"),
                    structfs_core_store::Record::parsed(
                        structfs_core_store::Value::Null,
                    ),
                )
                .await
                .ok();
        }
    }
}
```

Handle `Esc` during wizard — show "Skip setup?" confirmation. For the first pass, just exit wizard and go to inbox:

```rust
if settings.wizard.is_some() && key_str == "Esc" && settings.editing.is_none() {
    settings.wizard = None;
    client
        .write(
            &path!("ui/go_to_inbox"),
            structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
        )
        .await
        .ok();
}
```

- [ ] **Step 5: Update settings_view for wizard overlay**

In `crates/ox-cli/src/settings_view.rs`, when `state.wizard.is_some()`, render a step indicator:

```rust
// In draw_settings, before the help line:
if let Some(ref step) = state.wizard {
    let step_text = match step {
        WizardStep::AddAccount => "Step 1/2: Add your first account",
        WizardStep::SetDefaults => "Step 2/2: Set your defaults",
        WizardStep::Done => "Setup complete! Press Enter to continue.",
    };
    // Render step_text as a highlighted line
}
```

- [ ] **Step 6: Run and verify**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles, tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "feat(ox-cli): ox init wizard and first-run detection

ox init subcommand launches TUI in wizard mode.
First-run detection: no keys found → wizard mode automatically.
Wizard walks through: add account → set defaults → inbox."
```

---

### Task 8: Defaults editing and model catalogs

**Files:**
- Modify: `crates/ox-cli/src/settings_state.rs`
- Modify: `crates/ox-cli/src/settings_view.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`

- [ ] **Step 1: Add hardcoded model catalogs**

In `crates/ox-cli/src/settings_state.rs`:

```rust
pub const ANTHROPIC_MODELS: &[&str] = &[
    "claude-sonnet-4-20250514",
    "claude-haiku-4-5-20251001",
];

pub const OPENAI_MODELS: &[&str] = &[
    "gpt-4o",
    "gpt-4o-mini",
];

/// Get model catalog for a dialect.
pub fn models_for_dialect(dialect: &str) -> &'static [&'static str] {
    match dialect {
        "openai" => OPENAI_MODELS,
        _ => ANTHROPIC_MODELS,
    }
}
```

- [ ] **Step 2: Handle defaults field editing**

In the event loop, when `settings.focus == SettingsFocus::Defaults`:
- `j`/`k` or `Down`/`Up` — navigate between account/model/max_tokens
- `Left`/`Right` on account picker — cycle through accounts
- `Left`/`Right` on model picker — cycle through models for default account's dialect
- Character/Backspace on max_tokens — edit the number string
- `Enter` — save defaults to config

- [ ] **Step 3: Update defaults rendering**

In `settings_view.rs`, update `draw_defaults_section` to show the actual model picker:

```rust
fn draw_defaults_section(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let block = Block::default()
        .title(" Defaults ")
        .borders(Borders::ALL)
        .border_style(if state.focus == SettingsFocus::Defaults {
            theme.active_border
        } else {
            theme.border
        });

    let acct_name = state
        .accounts
        .get(state.default_account_idx)
        .map(|a| a.name.as_str())
        .unwrap_or("(none)");

    let dialect = state
        .accounts
        .get(state.default_account_idx)
        .map(|a| a.dialect.as_str())
        .unwrap_or("anthropic");
    let models = models_for_dialect(dialect);
    let model_name = models
        .get(state.default_model_idx)
        .unwrap_or(&"(unknown)");

    let is_defaults = state.focus == SettingsFocus::Defaults;
    let lines = vec![
        Line::from(format!(
            "  Account:    {}{acct_name}",
            if is_defaults && state.defaults_focus == 0 { "▸ " } else { "  " }
        )),
        Line::from(format!(
            "  Model:      {}{model_name}",
            if is_defaults && state.defaults_focus == 1 { "▸ " } else { "  " }
        )),
        Line::from(format!(
            "  Max tokens: {}{}",
            if is_defaults && state.defaults_focus == 2 { "▸ " } else { "  " },
            state.default_max_tokens
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
```

- [ ] **Step 4: Save defaults to config**

When Enter is pressed in defaults section, write to config.toml:

```rust
// Save defaults
let acct_name = settings.accounts.get(settings.default_account_idx)
    .map(|a| a.name.clone())
    .unwrap_or_default();
let dialect = settings.accounts.get(settings.default_account_idx)
    .map(|a| a.dialect.as_str())
    .unwrap_or("anthropic");
let models = models_for_dialect(dialect);
let model = models.get(settings.default_model_idx)
    .unwrap_or(&"claude-sonnet-4-20250514");
let max_tokens: i64 = settings.default_max_tokens.parse().unwrap_or(4096);

// Write to ConfigStore via broker
client.write(
    &path!("config/gate/defaults/account"),
    Record::parsed(Value::String(acct_name)),
).await.ok();
client.write(
    &path!("config/gate/defaults/model"),
    Record::parsed(Value::String(model.to_string())),
).await.ok();
client.write(
    &path!("config/gate/defaults/max_tokens"),
    Record::parsed(Value::Integer(max_tokens)),
).await.ok();
// Persist
client.write(
    &path!("config/save"),
    Record::parsed(Value::Null),
).await.ok();
```

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "feat(ox-cli): defaults editing with model catalog picker

Hardcoded model catalogs per dialect.
Left/Right to cycle accounts and models.
Enter saves defaults to config."
```

---

### Task 9: Quality gates and status doc

**Files:**
- Run: `./scripts/quality_gates.sh`
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Run formatter**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: all 14 gates pass. Fix any failures.

- [ ] **Step 3: Update status doc**

Add after the Phase 5 entry in `docs/design/rfc/structfs-tui-status.md`:

```markdown
#### Phase 6: Init + Settings Screen (complete, 14/14 quality gates)
- Key files in ~/.ox/keys/ (0700 permissions) replace key field on AccountConfig/AccountEntry
- resolve_keys() loads from key files and env vars, injected into flat config map
- Screen::Settings variant with account CRUD (add/edit/delete)
- Account fields: name, dialect (anthropic/openai), endpoint (custom URL), API key
- Test connection: minimal completion to verify key works
- Wizard mode for ox init and first-run detection
- Defaults editing: account/model/max_tokens pickers with hardcoded catalogs
- ox init subcommand, first-run auto-detection (no keys → wizard)
- Removed hard exit(1) on missing key
- **Spec:** `docs/superpowers/specs/2026-04-08-init-settings-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-init-settings.md`
```

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "docs: update status for Phase 6 init + settings screen"
```
