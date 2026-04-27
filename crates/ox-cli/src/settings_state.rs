//! Local state for the Settings screen.
//!
//! Owned by the event loop, not stored in the broker (ephemeral UI state).

use crate::simple_input::SimpleInput;
use tokio::sync::oneshot;

/// Result of an async test connection + model fetch.
pub struct TestResult {
    pub test: Result<(String, u128), String>, // (dialect, elapsed_ms) or error
    pub models: Result<Vec<ox_kernel::ModelInfo>, String>,
}

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
#[derive(Debug, Clone)]
pub struct AccountEditFields {
    pub name: SimpleInput,
    pub dialect: usize, // 0=anthropic, 1=openai
    pub endpoint: SimpleInput,
    pub key: SimpleInput,
    pub focus: usize, // 0=name, 1=dialect, 2=endpoint, 3=key
    pub is_new: bool,
}

impl AccountEditFields {
    /// Return a mutable reference to the SimpleInput for the currently focused
    /// text field, or None if the focused field is not a text field (e.g. dialect).
    pub fn focused_input(&mut self) -> Option<&mut SimpleInput> {
        match self.focus {
            0 => Some(&mut self.name),
            2 => Some(&mut self.endpoint),
            3 => Some(&mut self.key),
            _ => None,
        }
    }
}

pub const DIALECTS: [&str; 2] = ["anthropic", "openai"];

/// Test connection status.
#[derive(Debug, Clone)]
pub enum TestStatus {
    Idle,
    Testing,
    Success(String),
    Failed(String),
}

/// Account summary for display.
#[derive(Debug, Clone)]
pub struct AccountSummary {
    pub name: String,
    pub dialect: String,
    pub endpoint_display: String,
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
    pub default_account_idx: usize,
    pub default_model: SimpleInput,
    pub default_max_tokens: SimpleInput,
    pub defaults_focus: usize,
    pub discovered_models: Vec<ox_kernel::ModelInfo>,
    pub model_picker_idx: Option<usize>,
    pub pending_test: Option<oneshot::Receiver<TestResult>>,
    pub delete_confirming: bool,
    pub save_flash_until: Option<std::time::Instant>,
    /// Scroll offset (in rows) for the status `TextPane`. Reset whenever
    /// `test_status` transitions so a new error always starts visible.
    /// Bumped by PageUp/PageDown / Ctrl+u / Ctrl+d while a status is shown.
    pub status_scroll: u16,
}

impl SettingsState {
    /// Replace `test_status` and reset the scroll offset, so a new message
    /// is always rendered from the top regardless of how the user had
    /// scrolled the previous one.
    pub fn set_status(&mut self, status: TestStatus) {
        self.test_status = status;
        self.status_scroll = 0;
    }

    pub fn new() -> Self {
        Self {
            focus: SettingsFocus::Accounts,
            selected_account: 0,
            accounts: Vec::new(),
            editing: None,
            test_status: TestStatus::Idle,
            wizard: None,
            default_account_idx: 0,
            default_model: SimpleInput::from("claude-sonnet-4-20250514"),
            default_max_tokens: SimpleInput::from("4096"),
            defaults_focus: 0,
            discovered_models: Vec::new(),
            model_picker_idx: None,
            pending_test: None,
            delete_confirming: false,
            save_flash_until: None,
            status_scroll: 0,
        }
    }

    /// Refresh the account list from config and resolved keys.
    pub fn refresh_accounts(
        &mut self,
        config: &crate::config::OxConfig,
        keys_dir: &std::path::Path,
    ) {
        let keys = crate::config::resolve_keys(keys_dir, config);
        let default_account = &config.gate.defaults.account;

        self.accounts = config
            .gate
            .accounts
            .iter()
            .map(|(name, entry)| {
                // Resolve the provider this account points at. Endpoint and
                // dialect both come from the provider table; "anthropic" and
                // "openai" are the built-in defaults when no entry exists.
                let provider_entry = config.gate.providers.get(&entry.provider);
                let dialect = provider_entry
                    .map(|p| p.dialect.clone())
                    .unwrap_or_else(|| entry.provider.clone());
                let endpoint = provider_entry
                    .map(|p| p.endpoint.clone())
                    .unwrap_or_else(|| match entry.provider.as_str() {
                        "anthropic" => "https://api.anthropic.com".into(),
                        "openai" => "https://api.openai.com".into(),
                        _ => String::new(),
                    });
                let endpoint_display = endpoint
                    .split("://")
                    .nth(1)
                    .unwrap_or(&endpoint)
                    .split('/')
                    .next()
                    .unwrap_or(&endpoint)
                    .to_string();
                AccountSummary {
                    name: name.clone(),
                    dialect,
                    endpoint_display,
                    has_key: keys.contains_key(name),
                    is_default: name == default_account,
                }
            })
            .collect();
        self.accounts.sort_by(|a, b| a.name.cmp(&b.name));

        self.default_account_idx = self.accounts.iter().position(|a| a.is_default).unwrap_or(0);
        self.default_model.set(&config.gate.defaults.model);
        self.default_max_tokens
            .set(&config.gate.defaults.max_tokens.to_string());

        if self.selected_account >= self.accounts.len() {
            self.selected_account = self.accounts.len().saturating_sub(1);
        }
    }

    pub fn new_wizard() -> Self {
        let mut s = Self::new();
        s.wizard = Some(WizardStep::AddAccount);
        s.editing = Some(AccountEditFields {
            name: SimpleInput::new(),
            dialect: 0,
            endpoint: SimpleInput::new(),
            key: SimpleInput::new(),
            focus: 0,
            is_new: true,
        });
        s
    }
}
