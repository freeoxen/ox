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
    pub default_model_idx: usize,
    pub default_max_tokens: String,
    pub defaults_focus: usize,
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
