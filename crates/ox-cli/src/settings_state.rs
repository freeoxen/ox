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
    pub dialect: usize, // 0=anthropic, 1=openai
    pub endpoint: String,
    pub key: String,
    pub focus: usize, // 0=name, 1=dialect, 2=endpoint, 3=key
    pub is_new: bool,
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
    pub default_model: String,
    pub default_max_tokens: String,
    pub defaults_focus: usize,
    pub discovered_models: Vec<ox_kernel::ModelInfo>,
    pub model_picker_idx: Option<usize>,
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
            default_model: "claude-sonnet-4-20250514".to_string(),
            default_max_tokens: "4096".to_string(),
            defaults_focus: 0,
            discovered_models: Vec::new(),
            model_picker_idx: None,
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
                let endpoint_display = entry
                    .endpoint
                    .as_ref()
                    .map(|ep| {
                        ep.split("://")
                            .nth(1)
                            .unwrap_or(ep)
                            .split('/')
                            .next()
                            .unwrap_or(ep)
                            .to_string()
                    })
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
        self.accounts.sort_by(|a, b| a.name.cmp(&b.name));

        self.default_account_idx = self
            .accounts
            .iter()
            .position(|a| a.is_default)
            .unwrap_or(0);
        self.default_model = config.gate.defaults.model.clone();
        self.default_max_tokens = config.gate.defaults.max_tokens.to_string();

        if self.selected_account >= self.accounts.len() {
            self.selected_account = self.accounts.len().saturating_sub(1);
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
