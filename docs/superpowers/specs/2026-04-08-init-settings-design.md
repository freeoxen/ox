# Init + Settings Screen Design

**Date:** 2026-04-08
**Status:** Draft
**Prereqs:** Phase 5 (accounts-first config redesign) complete

## Problem

There's no way to configure ox without hand-editing TOML or passing large env
vars. If no API key is configured, the CLI hard-exits with an error — no TUI,
no guidance. First-time users hit a wall.

## Design

### Data Model

**Config file** (`~/.ox/config.toml`): accounts + defaults. No secrets.

```toml
[gate.accounts.personal]
provider = "anthropic"

[gate.accounts.work-proxy]
provider = "anthropic"
endpoint = "https://llm.corp.internal/v1"

[gate.accounts.ollama]
provider = "openai"
endpoint = "http://localhost:11434/v1/chat/completions"

[gate.defaults]
account = "personal"
model = "claude-sonnet-4-20250514"
max_tokens = 4096
```

**Key files** (`~/.ox/keys/{account_name}.key`): one file per account, contains
the raw API key text. Directory created with `0700` permissions.

- Agent policy denies reads into `~/.ox/keys/` by default.
- Key files are never written to config.toml, never included in snapshots,
  never persisted by ConfigStore.

**Key resolution order** (highest wins):
1. Env var: `OX_GATE__ACCOUNTS__{NAME}__KEY=...`
2. Key file: `~/.ox/keys/{name}.key`

If neither exists, the account has no key.

**`AccountEntry` loses its `key` field.** Keys come from key files or env vars,
not TOML deserialization. The figment type becomes:

```rust
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountEntry {
    pub provider: String,
    #[serde(default)]
    pub endpoint: Option<String>,
}
```

**`AccountConfig` in ox-gate loses its `key` field.** Key is resolved at
runtime by the config/key-file layer and injected into the flat config map
as `gate/accounts/{name}/key`. GateStore reads it from its config handle
as before — no GateStore changes needed.

### Account Fields

Each account has:

| Field    | Required | Description                                          |
|----------|----------|------------------------------------------------------|
| Name     | yes      | Identifier, used as key file name and config key     |
| Dialect  | yes      | Wire format: `anthropic` or `openai`                 |
| Endpoint | no       | API URL override. Defaults to standard URL for dialect |
| API Key  | yes*     | Stored in `~/.ox/keys/{name}.key`, not in config     |

\* Required for test connection. Account can be saved without a key.

Dialect maps to the existing `ProviderConfig` struct (`dialect`, `endpoint`,
`version`). If endpoint is blank, uses the default for the dialect.

### Entry Points

Three entry points, one destination:

**1. `ox init`** — clap subcommand. Launches TUI directly into Settings screen
in wizard mode. On completion, transitions to Inbox.

**2. First-run detection** — `ox` (no subcommand) checks for accounts with key
files or env-var keys. If none found, launches into Settings in wizard mode
instead of hard-exiting. Same flow as `ox init` but automatic.

**3. `s` from Inbox** — enters Settings in edit mode (free-form, not
step-by-step). `Esc` or `q` returns to Inbox.

### Settings Screen

New `Screen::Settings` variant in UiStore.

**Edit mode** (from Inbox):

```
┌─ Settings ──────────────────────────────────────────┐
│                                                      │
│  Accounts                                            │
│  ┌──────────────────────────────────────────────┐   │
│  │ ● personal     anthropic  api.anthropic.com  ✓│   │
│  │   work-proxy   anthropic  llm.corp.internal  ✓│   │
│  │   ollama       openai     localhost:11434     ✗│   │
│  │                                                │   │
│  │   [a]dd  [e]dit  [d]elete  [t]est connection  │   │
│  └──────────────────────────────────────────────┘   │
│                                                      │
│  Defaults                                            │
│  ┌──────────────────────────────────────────────┐   │
│  │  Account:    personal  ▾                       │   │
│  │  Model:      claude-sonnet-4-20250514  ▾       │   │
│  │  Max tokens: 4096                              │   │
│  └──────────────────────────────────────────────┘   │
│                                                      │
│  [Esc] back to inbox                                 │
└──────────────────────────────────────────────────────┘
```

- `●` marks the default account.
- `✓`/`✗` indicates whether a key file exists.
- Account list shows: name, dialect, endpoint hostname, key status.
- Defaults section: dropdown-style pickers for account and model, editable
  integer for max_tokens.
- Model picker shows hardcoded catalog for the selected account's dialect.

**Account add/edit dialog:**

```
┌─ Add Account ────────────────────────────────────┐
│                                                    │
│  Name:      work-proxy                             │
│  Dialect:   anthropic ▾  (anthropic / openai)      │
│  Endpoint:  https://llm.corp.internal/v1           │
│  API Key:   ●●●●●●●●●●sk-...last4                 │
│                                                    │
│  [t]est connection  [Enter] save  [Esc] cancel     │
└────────────────────────────────────────────────────┘
```

- Key input is masked (shows last 4 chars).
- On save: writes account to config.toml, writes key to `~/.ox/keys/{name}.key`.
- Test connection: sends minimal completion request, shows spinner → ✓/✗.

### Wizard Mode

Wizard mode is the Settings screen with a guided overlay. It walks through:

1. **Add account** — opens account dialog pre-focused. Provider selection →
   key entry → test connection.
2. **Set defaults** — auto-populated from first account. User can adjust model
   and max_tokens.
3. **Done** — "Ready to go" confirmation, transitions to Inbox.

If the user presses `Esc` during wizard, they get a confirmation: "Skip setup?
You can configure later with `ox init` or press `s` from the inbox." On
confirm, proceeds to Inbox with whatever is configured (may have no accounts).

### Test Connection

Sends a minimal completion to verify the key works:

- System prompt: empty
- User message: "hi"
- Max tokens: 1
- Model: cheapest available for the dialect (claude-haiku for anthropic,
  gpt-4o-mini for openai), or the user's selected default model

Shows a spinner while waiting. On success: `✓ Connected (anthropic, 200ms)`.
On failure: `✗ Error: {message}` (e.g. "invalid API key", "connection refused").

Uses the existing transport layer (`crate::transport::make_send_fn` in ox-cli)
with a `ProviderConfig` constructed from the account's dialect + endpoint.

### Config Persistence

**On account save:**
1. Write account entry to `~/.ox/config.toml` (provider + optional endpoint,
   no key).
2. Write key to `~/.ox/keys/{name}.key` (create `keys/` dir with `0700` if
   needed).
3. Trigger ConfigStore reload via broker `config/save` command.

**On account delete:**
1. Remove account entry from config.toml.
2. Delete `~/.ox/keys/{name}.key`.
3. If deleted account was the default, set default to first remaining account.

**On defaults change:**
1. Write to ConfigStore runtime layer via broker.
2. Persist via `config/save` command.

### CLI Changes

```rust
#[derive(Parser)]
#[command(name = "ox", about = "Agentic coding CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Named account from config
    #[arg(long)]
    account: Option<String>,

    /// Model identifier
    #[arg(long, short)]
    model: Option<String>,

    /// Workspace root directory
    #[arg(long, default_value = ".")]
    workspace: String,

    /// Max tokens per completion
    #[arg(long)]
    max_tokens: Option<u32>,

    /// Disable policy enforcement
    #[arg(long)]
    no_policy: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive setup wizard
    Init,
}
```

**Startup flow changes:**

```
ox init        → launch TUI in wizard mode
ox (no args)   → resolve config
                  → if no account has a key: wizard mode
                  → else: normal startup (Inbox)
```

The hard `exit(1)` on missing key is removed. Replaced by wizard-mode entry.

### Key File Resolution

New function in `ox-cli/src/config.rs`:

```rust
pub fn resolve_keys(
    keys_dir: &Path,
    config: &mut OxConfig,
) -> BTreeMap<String, String> {
    let mut keys = BTreeMap::new();
    for (name, _entry) in &config.gate.accounts {
        // Env var takes precedence
        let env_key = format!("OX_GATE__ACCOUNTS__{}_KEY",
            name.to_uppercase());
        if let Ok(k) = std::env::var(&env_key) {
            if !k.is_empty() {
                keys.insert(name.clone(), k);
                continue;
            }
        }
        // Key file fallback
        let key_path = keys_dir.join(format!("{name}.key"));
        if let Ok(contents) = std::fs::read_to_string(&key_path) {
            let trimmed = contents.trim().to_string();
            if !trimmed.is_empty() {
                keys.insert(name.clone(), trimmed);
            }
        }
    }
    keys
}
```

Keys are injected into the flat config map as `gate/accounts/{name}/key`
before ConfigStore initialization. GateStore reads them from its config handle
unchanged.

### Policy

Default policy denies agent tool calls that read from `~/.ox/keys/`:

```
deny read_file ~/.ox/keys/*
deny shell cat ~/.ox/keys/*
```

This is enforced by the existing `PolicyGuard` in `ox-cli/src/policy.rs`.

### What This Removes

- `key` field from `AccountEntry` (figment type in ox-cli/src/config.rs)
- `key` field from `AccountConfig` (ox-gate/src/account.rs)
- Hard `exit(1)` on missing API key in main.rs
- `OX_GATE__ACCOUNTS__{NAME}__KEY` in config.toml (keys never in TOML)

### What This Adds

- `ox init` subcommand
- `Screen::Settings` variant in UiStore
- Settings screen with account CRUD + defaults editing
- Wizard mode for first-run / init
- Key file storage in `~/.ox/keys/`
- `endpoint` field on `AccountEntry`
- `resolve_keys()` function for key file + env var resolution
- Test connection flow
- `s` keybinding from Inbox to Settings
- Policy rules denying agent access to keys directory

## Execution Order

1. Key file infrastructure: `resolve_keys()`, remove `key` from AccountEntry/AccountConfig, key file read/write, `keys/` dir creation with permissions
2. Settings store: new `SettingsStore` or extend UiStore with settings state (account list, selection, editing fields, wizard step)
3. Settings screen: `Screen::Settings`, rendering, keybindings, account list display
4. Account CRUD: add/edit/delete dialogs, config.toml write, key file write
5. Defaults editing: account picker, model picker, max_tokens field
6. Test connection: minimal completion, spinner, result display
7. Wizard mode: guided overlay, step sequencing, first-run detection
8. CLI changes: `ox init` subcommand, remove hard exit, startup flow
9. Policy: default deny rules for keys directory
10. Quality gates + status doc
