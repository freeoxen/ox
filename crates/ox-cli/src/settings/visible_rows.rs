//! Compute the visible row list for the settings tree view.
//!
//! The settings screen renders an accordion: top-level entries
//! (Accounts, Models) are always visible; their children appear
//! inline only when the entry is in the expanded set. Both the
//! renderer (which draws the tree) and the navigation commands
//! (which walk j/k between rows) consult this enumeration so the
//! visible-row order can never disagree between them.

use ox_path::oxpath;
use ox_types::{AccountField, ModelField, SettingsIndexEntry};
use structfs_core_store::{Path, Reader, Value};

use super::renderers::util::{child_names_under, read_typed};

/// What category a row represents — drives both the rendering shape
/// (label decoration, badge handling) and the binding behavior
/// (Enter on a top-level entry toggles, Enter on a leaf descends).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A top-level index entry: Accounts, Models, …
    Entry { entry_id: String },
    /// One account row inside an expanded Accounts entry.
    Account { name: String },
    /// One (account, model_id) row inside an expanded Models entry.
    Model { account: String, model_id: String },
    /// Synthetic placeholder when a connection has no cataloged models.
    /// Activating it (Enter) triggers a catalog refresh for the named
    /// connection — gives the user a discoverable next action where the
    /// natural one ("expand to see models") would otherwise yield a
    /// silent zero-row expansion.
    ModelEmptyState { account: String },
    /// Inline "+ add model manually" row that appears directly under a
    /// ModelEmptyState row. Activating it opens the three-field manual
    /// entry form. Future iteration may also emit this after the last
    /// model row of a non-empty catalog so users can add custom entries
    /// alongside auto-enumerated ones.
    ModelAddManual { account: String },
    /// One field row under an expanded account.
    AccountField {
        account: String,
        field: AccountField,
    },
    /// One field row under an expanded model.
    ModelField {
        account: String,
        model_id: String,
        field: ModelField,
    },
}

/// One row of the visible tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleRow {
    pub path: Path,
    pub depth: usize,
    pub label: String,
    /// Right-aligned secondary text (e.g. model token-budget summary).
    /// `None` when the row has no extra metadata to show.
    pub secondary: Option<String>,
    pub badge: Option<String>,
    pub kind: RowKind,
    pub expandable: bool,
    pub expanded: bool,
}

/// Read the expanded set from `ui/settings/expanded`. Stored as a
/// `Vec<String>` of cursor-path-strings; absent / wrong-shape reads
/// return empty (nothing expanded).
pub fn read_expanded_set(data: &mut dyn Reader) -> Vec<String> {
    read_typed::<Vec<String>>(data, &oxpath!("ui", "settings", "expanded")).unwrap_or_default()
}

/// Encode an expanded set back to a `Value` for writing.
pub fn expanded_set_to_value(set: &[String]) -> Value {
    Value::Array(set.iter().cloned().map(Value::String).collect())
}

/// Enumerate every visible row in the tree, top to bottom.
///
/// Top-level entries come from `settings/index/entries` (the same data
/// the legacy index renderer read). Each entry's children are
/// enumerated only when the entry's `target_cursor` (rendered as
/// string) is in the expanded set.
pub fn enumerate(data: &mut dyn Reader) -> Vec<VisibleRow> {
    let mut rows: Vec<VisibleRow> = Vec::new();
    let expanded = read_expanded_set(data);
    let entry_ids = child_names_under(data, "settings/index/entries");

    for id in &entry_ids {
        let comp = match ox_kernel::PathComponent::try_new(id) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let entry: SettingsIndexEntry =
            match read_typed(data, &oxpath!("settings", "index", "entries", comp)) {
                Some(e) => e,
                None => continue,
            };
        let path = entry.target_cursor.clone();
        let path_str = path_to_string(&path);
        let is_expanded = expanded.iter().any(|s| s == &path_str);

        rows.push(VisibleRow {
            path: path.clone(),
            depth: 0,
            label: entry.label,
            secondary: None,
            badge: resolve_badge(data, &entry.badge),
            kind: RowKind::Entry {
                entry_id: entry.id.clone(),
            },
            expandable: true,
            expanded: is_expanded,
        });

        if is_expanded {
            match entry.id.as_str() {
                "accounts" => append_account_rows(&mut rows, data, &expanded),
                "models" => append_model_rows(&mut rows, data, &expanded),
                _ => {}
            }
        }
    }
    rows
}

/// Append one row per account in `config/gate/accounts/*`. Account
/// rows are *expandable* — when in the expanded set, their fields
/// (Name / Protocol / Endpoint / Auth / Key) appear inline as
/// depth-2 rows. The user never has to leave `settings/index` to see
/// or act on an account.
fn append_account_rows(rows: &mut Vec<VisibleRow>, data: &mut dyn Reader, expanded: &[String]) {
    use ox_gate::AccountConfig;

    let names = child_names_under(data, "config/gate/accounts");
    // Pre-compute the provider-to-accounts map so the share-set lookup
    // is one pass, not N×N.
    let mut provider_users: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for n in &names {
        let provider = if let Ok(comp) = ox_kernel::PathComponent::try_new(n) {
            let acct: Option<AccountConfig> =
                read_typed(data, &oxpath!("config", "gate", "accounts", comp));
            acct.map(|a| a.provider)
                .or_else(|| read_account_child_string_in_visible_rows(data, n, "provider"))
        } else {
            None
        };
        if let Some(p) = provider {
            provider_users.entry(p).or_default().push(n.clone());
        }
    }

    for name in &names {
        let comp = match ox_kernel::PathComponent::try_new(name) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let acct: AccountConfig =
            read_typed(data, &oxpath!("config", "gate", "accounts", comp.clone())).unwrap_or_else(
                || AccountConfig {
                    provider: read_account_child_string_in_visible_rows(data, name, "provider")
                        .unwrap_or_else(|| "anthropic".to_string()),
                },
            );

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

/// Read a child string under `config/gate/accounts/{name}/{child}`
/// — mirrors `read_account_child_string` in `account_model.rs`. The
/// fallback covers TOML-loaded accounts that store fields as separate
/// leaves rather than as one AccountConfig record.
fn read_account_child_string_in_visible_rows(
    data: &mut dyn Reader,
    account: &str,
    child: &str,
) -> Option<String> {
    let acct_comp = ox_kernel::PathComponent::try_new(account).ok()?;
    let child_comp = ox_kernel::PathComponent::try_new(child).ok()?;
    let r = data
        .read(&oxpath!(
            "config", "gate", "accounts", acct_comp, child_comp
        ))
        .ok()
        .flatten()?;
    match r.as_value()? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Append one row per (account, model_id) pair. Like accounts, model
/// rows are expandable — expanding shows the per-model overrides.
fn append_model_rows(rows: &mut Vec<VisibleRow>, data: &mut dyn Reader, expanded: &[String]) {
    use ox_types::settings::ModelKey;

    // Resolve the bootstrap role once per enumeration so every row can
    // be cheaply tagged. Same fallback the badge resolver uses: new path
    // first, legacy path second, so freshly-installed and upgraded
    // installs both render the badge against whichever path holds the
    // user's choice.
    let bootstrap: Option<ox_gate::CompletionRole> =
        read_typed(data, &oxpath!("config", "gate", "completions", "bootstrap"))
            .or_else(|| read_typed(data, &oxpath!("config", "gate", "completions", "primary")));
    // Same one-shot read for the default-available set: each row tests
    // membership locally instead of re-reading the record per pair.
    // Absent record collapses to an empty vec — no rows badge with D
    // until the user explicitly opts a subset in.
    let default_set: Vec<ModelKey> = read_typed(
        data,
        &oxpath!("config", "gate", "completions", "default_available"),
    )
    .unwrap_or_default();

    let account_names = child_names_under(data, "config/gate/accounts");
    for account_name in &account_names {
        // `child_names_under` splits broker keys on `/`, so its outputs
        // never contain `/`. Account names live in the broker as path
        // components (the broker rejects non-identifiers on write), so
        // any name we see here is already valid as a path component.
        let models_path = Path::try_from_components(vec![
            "config".to_string(),
            "gate".to_string(),
            "accounts".to_string(),
            account_name.clone(),
            "models".to_string(),
        ])
        .expect("account names from child_names_under are valid path components");
        let models: Vec<ox_gate::ModelInfo> = read_typed(data, &models_path).unwrap_or_default();

        if models.is_empty() {
            let path = row_path(&[
                "settings",
                "models",
                &safe_component(account_name),
                "_empty",
            ]);
            rows.push(VisibleRow {
                path,
                depth: 1,
                label: format!("{} / (no models — Enter to refresh)", account_name),
                secondary: None,
                badge: None,
                kind: RowKind::ModelEmptyState {
                    account: account_name.clone(),
                },
                expandable: false,
                expanded: false,
            });
            let add_path = row_path(&["settings", "models", &safe_component(account_name), "_add"]);
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

        for m in models {
            let path = row_path(&[
                "settings",
                "models",
                &safe_component(account_name),
                &safe_component(&m.id),
            ]);
            let path_str = path_to_string(&path);
            let is_expanded = expanded.iter().any(|s| s == &path_str);
            let is_bootstrap = bootstrap
                .as_ref()
                .is_some_and(|r| r.account == *account_name && r.model_id == m.id);
            let is_default = default_set
                .iter()
                .any(|k| k.account == *account_name && k.model_id == m.id);
            // D before B: default-available is the multi-select common
            // case the user toggles often, bootstrap is the single
            // global pin. Reading "D B" left-to-right matches that
            // mental order.
            let badge = match (is_default, is_bootstrap) {
                (true, true) => Some("D B".to_string()),
                (true, false) => Some("D".to_string()),
                (false, true) => Some("B".to_string()),
                (false, false) => None,
            };
            rows.push(VisibleRow {
                path: path.clone(),
                depth: 1,
                label: format!("{} / {}", account_name, m.id),
                secondary: Some(model_secondary(&m)),
                badge,
                kind: RowKind::Model {
                    account: account_name.clone(),
                    model_id: m.id.clone(),
                },
                expandable: true,
                expanded: is_expanded,
            });
            if is_expanded {
                append_model_field_rows(rows, &m, account_name);
            }
        }
    }
}

/// Field rows for an expanded account. Reads each field's current
/// value from the broker; the renderer formats them as `"Label: value"`
/// rows so a user can see the whole account state at a glance.
fn append_account_field_rows(rows: &mut Vec<VisibleRow>, data: &mut dyn Reader, name: &str) {
    use ox_gate::{AccountConfig, ApiKey, ProviderConfig};

    let comp = match ox_kernel::PathComponent::try_new(name) {
        Ok(c) => c,
        Err(_) => return,
    };
    let acct: AccountConfig =
        read_typed(data, &oxpath!("config", "gate", "accounts", comp.clone())).unwrap_or_default();
    let provider: Option<ProviderConfig> = ox_kernel::PathComponent::try_new(&acct.provider)
        .ok()
        .and_then(|pc| read_typed(data, &oxpath!("config", "gate", "providers", pc)));
    let key: Option<ApiKey> = read_typed(data, &oxpath!("secret", "keys", comp.clone()));

    for field in [
        AccountField::Name,
        AccountField::Protocol,
        AccountField::Endpoint,
        AccountField::Auth,
        AccountField::Key,
    ] {
        let value = match field {
            AccountField::Name => name.to_string(),
            // Protocol = the dialect the bound provider speaks, not the
            // provider record's name. When the record is missing (orphan
            // binding) fall back to acct.provider, which for legacy
            // direct-cloud accounts doubles as the dialect.
            AccountField::Protocol => provider
                .as_ref()
                .map(|p| p.dialect.clone())
                .unwrap_or_else(|| acct.provider.clone()),
            AccountField::Endpoint => provider
                .as_ref()
                .map(|p| p.endpoint.clone())
                .unwrap_or_default(),
            AccountField::Auth => provider
                .as_ref()
                .map(|p| match &p.auth {
                    Some(scheme) => format!("{scheme:?}").to_lowercase(),
                    None => String::from("(default)"),
                })
                .unwrap_or_default(),
            AccountField::Key => match key.as_ref() {
                Some(_) => "(set)".to_string(),
                None => "(unset)".to_string(),
            },
        };
        let label = match field {
            AccountField::Name => "Name",
            AccountField::Protocol => "Protocol",
            AccountField::Endpoint => "Endpoint",
            AccountField::Auth => "Auth",
            AccountField::Key => "Key",
        };
        let path = row_path(&[
            "settings",
            "accounts",
            &safe_component(name),
            field_segment_account(field),
        ]);
        rows.push(VisibleRow {
            path,
            depth: 2,
            label: format!("{label}: {value}"),
            secondary: None,
            badge: None,
            kind: RowKind::AccountField {
                account: name.to_string(),
                field,
            },
            expandable: false,
            expanded: false,
        });
    }
}

/// Field rows for an expanded model. Surfaces the two overridable
/// token-window fields plus the read-only id and display name so the
/// user has the full picture before opening the editor.
fn append_model_field_rows(rows: &mut Vec<VisibleRow>, model: &ox_gate::ModelInfo, account: &str) {
    let render = |opt: Option<u32>| match opt {
        Some(n) => n.to_string(),
        None => "—".to_string(),
    };
    for (field, label, value) in [
        (
            ModelField::ContextSizeOverride,
            "max_context_size",
            render(model.max_context_size),
        ),
        (
            ModelField::OutputTokensOverride,
            "max_output_tokens",
            render(model.max_output_tokens),
        ),
    ] {
        let path = row_path(&[
            "settings",
            "models",
            &safe_component(account),
            &safe_component(&model.id),
            field_segment_model(field),
        ]);
        rows.push(VisibleRow {
            path,
            depth: 2,
            label: format!("{label}: {value}"),
            secondary: None,
            badge: None,
            kind: RowKind::ModelField {
                account: account.to_string(),
                model_id: model.id.clone(),
                field,
            },
            expandable: false,
            expanded: false,
        });
    }
}

fn field_segment_account(field: AccountField) -> &'static str {
    match field {
        AccountField::Name => "name",
        AccountField::Protocol => "protocol",
        AccountField::Endpoint => "endpoint",
        AccountField::Auth => "auth",
        AccountField::Key => "key",
    }
}

fn field_segment_model(field: ModelField) -> &'static str {
    match field {
        ModelField::ContextSizeOverride => "max_context_size",
        ModelField::OutputTokensOverride => "max_output_tokens",
    }
}

/// Build a row identifier `Path` from a slice of component strings.
/// Total: every caller passes statically-known prefix segments
/// (`"settings"`, `"accounts"`, `"models"`) plus user-derived strings
/// pre-sanitized through [`safe_component`]. The combination always
/// satisfies UAX#31, so the construction can't fail — `expect` here
/// pins the invariant rather than silently dropping rows.
fn row_path(parts: &[&str]) -> Path {
    let owned: Vec<String> = parts.iter().map(|s| (*s).to_string()).collect();
    Path::try_from_components(owned)
        .expect("row_path callers always supply identifier-safe components")
}

/// Sanitize a free-form identifier (account name, model id) into a
/// component the path validator accepts. Replaces every char that
/// would be rejected by UAX#31 identifier rules with `_`. The result
/// only needs to be stable and unique enough to identify the row in
/// the visible list — the real identifier lives on `RowKind`, which
/// commands consult before issuing any data write.
pub(crate) fn safe_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut empty = true;
    for (i, ch) in s.chars().enumerate() {
        let ok = if i == 0 {
            ch.is_alphabetic() || ch == '_'
        } else {
            ch.is_alphanumeric() || ch == '_'
        };
        out.push(if ok { ch } else { '_' });
        empty = false;
    }
    if empty {
        out.push('_');
    }
    out
}

/// Resolve a badge to a display string for tree rendering.
fn resolve_badge(data: &mut dyn Reader, source: &ox_types::BadgeSource) -> Option<String> {
    use crate::settings::renderers::util::subtree_count;
    use ox_gate::CompletionRole;
    use ox_types::BadgeSource;

    match source {
        BadgeSource::None => None,
        BadgeSource::Static(s) => Some(s.clone()),
        BadgeSource::SubtreeCount(p) => Some(subtree_count(data, &p.to_string()).to_string()),
        BadgeSource::PrimaryReference => {
            read_typed::<CompletionRole>(data, &oxpath!("config", "gate", "completions", "primary"))
                .map(|role| format!("{} / {}", role.account, role.model_id))
        }
        BadgeSource::BootstrapReference => read_typed::<CompletionRole>(
            data,
            &oxpath!("config", "gate", "completions", "bootstrap"),
        )
        .or_else(|| {
            read_typed::<CompletionRole>(data, &oxpath!("config", "gate", "completions", "primary"))
        })
        .map(|role| format!("{} / {}", role.account, role.model_id)),
    }
}

/// Stringify a `Path` for storage in the expanded set. The set is a
/// `Vec<String>` because `Path` doesn't implement `Serialize`; we
/// project to its slash-joined wire form, which round-trips through
/// `Path::parse`.
pub fn path_to_string(p: &Path) -> String {
    p.to_string()
}

/// Find the visible row whose path matches `cursor`, returning its
/// index in the visible-row list. `None` when the focus has stale
/// drift (e.g. the row collapsed away under it); callers fall back
/// to the first row.
pub fn position_of(rows: &[VisibleRow], cursor: &Path) -> Option<usize> {
    rows.iter().position(|r| &r.path == cursor)
}

/// Format a token count for display. Uses `k` / `M` suffixes above
/// 1000 / 1_000_000; raw decimal below. Mirrors how model docs and
/// dashboards label context windows ("200k context") so the rendered
/// secondary text reads naturally to anyone who has read a model card.
fn format_token_count(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// Format a model's secondary metadata line: token budgets, dashed
/// when unknown.
fn model_secondary(m: &ox_gate::ModelInfo) -> String {
    let ctx = m
        .max_context_size
        .map(format_token_count)
        .unwrap_or_else(|| "—".to_string());
    let out = m
        .max_output_tokens
        .map(format_token_count)
        .unwrap_or_else(|| "—".to_string());
    format!("ctx {ctx} · out {out}")
}

#[cfg(test)]
mod tests {
    use super::*;

    use ox_gate::AccountConfig;
    use ox_gate::{ModelInfo, ModelInfoSource};
    use ox_types::{BadgeSource, SettingsIndexEntry};
    use structfs_serde_store::to_value;

    use crate::settings::snapshot::SettingsSnapshot;

    fn entry(id: &str, label: &str, target: &str, badge: BadgeSource) -> SettingsIndexEntry {
        SettingsIndexEntry {
            id: id.to_string(),
            label: label.to_string(),
            description: String::new(),
            target_cursor: Path::parse(target).unwrap(),
            badge,
        }
    }

    fn write_account(snap: &mut SettingsSnapshot, name: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: name.into(),
            })
            .unwrap(),
        );
    }

    fn write_account_with_provider(snap: &mut SettingsSnapshot, name: &str, provider: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: provider.into(),
            })
            .unwrap(),
        );
    }

    fn write_account_with_models(snap: &mut SettingsSnapshot, name: &str, ids: &[&str]) {
        write_account(snap, name);
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        let models: Vec<ModelInfo> = ids
            .iter()
            .map(|id| ModelInfo {
                id: (*id).into(),
                display_name: (*id).into(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            })
            .collect();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&models).unwrap(),
        );
    }

    fn write_index_entries(snap: &mut SettingsSnapshot) {
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&entry(
                "accounts",
                "Accounts",
                "settings/accounts",
                BadgeSource::SubtreeCount(Path::parse("config/gate/accounts").unwrap()),
            ))
            .unwrap(),
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&entry(
                "models",
                "Models",
                "settings/models",
                BadgeSource::PrimaryReference,
            ))
            .unwrap(),
        );
    }

    #[test]
    fn nothing_expanded_yields_only_top_level() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account(&mut snap, "alpha");
        let rows = enumerate(&mut snap);
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0].kind, RowKind::Entry { entry_id } if entry_id == "accounts"));
        assert_eq!(rows[0].depth, 0);
        assert!(matches!(&rows[1].kind, RowKind::Entry { entry_id } if entry_id == "models"));
    }

    #[test]
    fn expanded_accounts_inlines_account_rows() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        let rows = enumerate(&mut snap);
        // Accounts header + 2 accounts + Models header = 4
        assert_eq!(rows.len(), 4);
        assert!(matches!(&rows[1].kind, RowKind::Account { name } if name == "alpha"));
        assert_eq!(rows[1].depth, 1);
        assert!(matches!(&rows[2].kind, RowKind::Account { name } if name == "beta"));
        assert!(matches!(&rows[3].kind, RowKind::Entry { entry_id } if entry_id == "models"));
    }

    #[test]
    fn account_row_secondary_indicates_shared_provider() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        // Three accounts, two share provider "anthropic", one uses "openai".
        write_account_with_provider(&mut snap, "personal", "anthropic");
        write_account_with_provider(&mut snap, "work", "anthropic");
        write_account_with_provider(&mut snap, "lab", "openai");
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

    #[test]
    fn expanded_models_inlines_model_pairs() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1", "m2"]);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        // Accounts header + Models header + 2 model pairs = 4
        assert_eq!(rows.len(), 4);
        assert!(matches!(
            &rows[2].kind,
            RowKind::Model { account, model_id } if account == "alpha" && model_id == "m1"
        ));
        assert!(matches!(
            &rows[3].kind,
            RowKind::Model { account, model_id } if account == "alpha" && model_id == "m2"
        ));
    }

    #[test]
    fn model_row_badge_marks_bootstrap_choice() {
        use ox_gate::CompletionRole;
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["claude-sonnet-4", "claude-opus-4"]);
        snap.insert(
            &oxpath!("config", "gate", "completions", "bootstrap"),
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
            .find(|r| {
                matches!(
                    &r.kind,
                    RowKind::Model { account, model_id }
                        if account == "alpha" && model_id == "claude-sonnet-4"
                )
            })
            .expect("bootstrap row");
        assert_eq!(bootstrap_row.badge.as_deref(), Some("B"));
        let other_row = rows
            .iter()
            .find(|r| {
                matches!(
                    &r.kind,
                    RowKind::Model { account, model_id }
                        if account == "alpha" && model_id == "claude-opus-4"
                )
            })
            .expect("non-bootstrap row");
        assert!(other_row.badge.is_none());
    }

    #[test]
    fn entry_id_with_invalid_path_component_is_skipped() {
        // child_names_under returns raw string segments; a deliberately
        // bogus key whose direct child contains hyphens fails
        // PathComponent::try_new and the entry is skipped. Without the
        // skip, enumerate would propagate an error or panic.
        let mut snap = SettingsSnapshot::empty();
        // Inject a garbage entry under a hyphenated id by going through
        // the inner store directly (the public broker API would reject
        // the path on the way in).
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&entry(
                "accounts",
                "Accounts",
                "settings/accounts",
                BadgeSource::None,
            ))
            .unwrap(),
        );
        // Hyphenated id at the entries prefix; child_names_under will
        // surface it because it splits on `/` without validating.
        snap.insert_raw(
            "settings/index/entries/bad-id".to_string(),
            Value::String("ignored".into()),
        );
        let rows = enumerate(&mut snap);
        // Only the valid `accounts` entry comes through.
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0].kind, RowKind::Entry { entry_id } if entry_id == "accounts"));
    }

    #[test]
    fn entry_with_unparseable_payload_is_skipped() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&entry(
                "accounts",
                "Accounts",
                "settings/accounts",
                BadgeSource::None,
            ))
            .unwrap(),
        );
        // Garbage shape at a valid id — read_typed returns None and the
        // entry is skipped without panicking.
        snap.insert(
            &oxpath!("settings", "index", "entries", "garbled"),
            Value::String("not a SettingsIndexEntry".into()),
        );
        let rows = enumerate(&mut snap);
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0].kind, RowKind::Entry { entry_id } if entry_id == "accounts"));
    }

    #[test]
    fn unknown_entry_id_does_not_recurse() {
        // An expanded entry whose id isn't `accounts` / `models` falls
        // through the catch-all match arm — no children appear under it.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("settings", "index", "entries", "appearance"),
            to_value(&entry(
                "appearance",
                "Appearance",
                "settings/appearance",
                BadgeSource::None,
            ))
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/appearance".to_string()]),
        );
        let rows = enumerate(&mut snap);
        // Only the entry row itself; no children appended.
        assert_eq!(rows.len(), 1);
        assert!(rows[0].expanded);
    }

    #[test]
    fn safe_component_substitutes_disallowed_chars() {
        assert_eq!(safe_component("claude-haiku-4"), "claude_haiku_4");
        assert_eq!(safe_component("a.b/c"), "a_b_c");
        // Empty input gets a placeholder so Path::try_from_components
        // never sees a zero-width segment.
        assert_eq!(safe_component(""), "_");
        // Leading digit is identifier-illegal in UAX#31 → underscored.
        assert_eq!(safe_component("4abc"), "_abc");
    }

    #[test]
    fn resolve_badge_static_returns_string() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&entry(
                "models",
                "Models",
                "settings/models",
                BadgeSource::Static("custom".into()),
            ))
            .unwrap(),
        );
        let rows = enumerate(&mut snap);
        assert_eq!(rows[0].badge.as_deref(), Some("custom"));
    }

    #[test]
    fn resolve_badge_none_yields_none() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&entry(
                "models",
                "Models",
                "settings/models",
                BadgeSource::None,
            ))
            .unwrap(),
        );
        let rows = enumerate(&mut snap);
        assert!(rows[0].badge.is_none());
    }

    #[test]
    fn resolve_badge_primary_reference_with_no_role_yields_none() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&entry(
                "models",
                "Models",
                "settings/models",
                BadgeSource::PrimaryReference,
            ))
            .unwrap(),
        );
        let rows = enumerate(&mut snap);
        assert!(rows[0].badge.is_none());
    }

    #[test]
    fn resolve_badge_bootstrap_reference_reads_new_path() {
        use ox_gate::CompletionRole;
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "completions", "bootstrap"),
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
        use ox_gate::CompletionRole;
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
        use ox_gate::CompletionRole;
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
            &oxpath!("config", "gate", "completions", "bootstrap"),
            to_value(&CompletionRole {
                account: "current".into(),
                model_id: "new-model".into(),
            })
            .unwrap(),
        );
        let badge = resolve_badge(&mut snap, &BadgeSource::BootstrapReference);
        assert_eq!(badge.as_deref(), Some("current / new-model"));
    }

    #[test]
    fn model_row_badge_marks_default_available_pair() {
        use ox_types::settings::ModelKey;
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1", "m2"]);
        snap.insert(
            &oxpath!("config", "gate", "completions", "default_available"),
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
            .find(|r| {
                matches!(
                    &r.kind,
                    RowKind::Model { account, model_id }
                        if account == "alpha" && model_id == "m1"
                )
            })
            .expect("m1 row");
        assert_eq!(m1_row.badge.as_deref(), Some("D"));
        let m2_row = rows
            .iter()
            .find(|r| {
                matches!(
                    &r.kind,
                    RowKind::Model { account, model_id }
                        if account == "alpha" && model_id == "m2"
                )
            })
            .expect("m2 row");
        assert!(m2_row.badge.is_none());
    }

    #[test]
    fn model_row_badge_combines_default_and_bootstrap() {
        use ox_gate::CompletionRole;
        use ox_types::settings::ModelKey;
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        snap.insert(
            &oxpath!("config", "gate", "completions", "default_available"),
            to_value(&vec![ModelKey {
                account: "alpha".into(),
                model_id: "m1".into(),
            }])
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "completions", "bootstrap"),
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

    #[test]
    fn empty_catalog_yields_one_empty_state_row_per_connection() {
        // Two accounts: one with a model, one with no catalog at all.
        // The Models section, when expanded, should show the cataloged
        // model row PLUS one synthetic ModelEmptyState row for the empty
        // connection — never silently zero rows for that connection.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        write_account(&mut snap, "beta"); // no models written
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        // Visible: [Accounts header, Models header, alpha/m1 row,
        // beta empty-state row, beta add-manual row] = 5
        assert_eq!(rows.len(), 5);
        let empty = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::ModelEmptyState { .. }))
            .expect("empty-state row for beta");
        match &empty.kind {
            RowKind::ModelEmptyState { account } => assert_eq!(account, "beta"),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(empty.depth, 1);
        assert!(!empty.expandable);
        assert!(empty.label.contains("no models"));
    }

    #[test]
    fn empty_catalog_row_has_unique_path_per_connection() {
        // Two empty connections must produce two distinct rows; their
        // paths must be unique so cursor tracking can distinguish them.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let empty: Vec<_> = rows
            .iter()
            .filter(|r| matches!(&r.kind, RowKind::ModelEmptyState { .. }))
            .collect();
        assert_eq!(empty.len(), 2);
        assert_ne!(empty[0].path, empty[1].path);
    }

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

    // -- format_token_count ---------------------------------------------

    #[test]
    fn format_token_count_uses_k_suffix_for_thousands() {
        assert_eq!(format_token_count(8_000), "8k");
        assert_eq!(format_token_count(200_000), "200k");
        assert_eq!(format_token_count(128_000), "128k");
    }

    #[test]
    fn format_token_count_uses_m_suffix_for_millions() {
        assert_eq!(format_token_count(1_000_000), "1M");
        assert_eq!(format_token_count(2_000_000), "2M");
    }

    #[test]
    fn format_token_count_uses_raw_decimal_below_1000() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(512), "512");
        assert_eq!(format_token_count(999), "999");
    }

    #[test]
    fn model_row_secondary_carries_ctx_and_out_metadata() {
        // A model row's secondary slot must surface the token budgets so
        // the user can compare models without drilling into each one.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "anthropic".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&vec![ModelInfo {
                id: "claude-sonnet-4".into(),
                display_name: "Claude Sonnet 4".into(),
                max_context_size: Some(200_000),
                max_output_tokens: Some(8_000),
                source: ModelInfoSource::Server,
            }])
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let model_row = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Model { .. }))
            .expect("model row");
        assert_eq!(model_row.secondary.as_deref(), Some("ctx 200k · out 8k"));
    }

    #[test]
    fn model_row_secondary_renders_em_dash_for_unknown_budget() {
        // A model entry whose catalog refresh got back ids only (no token
        // limits) and whose known-family table didn't fill them in must
        // still render a legible secondary — the dashes carry "we know
        // this model exists but not its budgets" without leaving the
        // slot blank.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "anthropic".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&vec![ModelInfo {
                id: "mystery-model".into(),
                display_name: "Mystery".into(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            }])
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let model_row = rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Model { .. }))
            .expect("model row");
        assert_eq!(model_row.secondary.as_deref(), Some("ctx — · out —"));
    }

    #[test]
    fn position_of_finds_visible_row() {
        let mut snap = SettingsSnapshot::empty();
        write_index_entries(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        let rows = enumerate(&mut snap);
        let alpha_path = oxpath!(
            "settings",
            "accounts",
            ox_kernel::PathComponent::try_new("alpha").unwrap()
        );
        assert_eq!(position_of(&rows, &alpha_path), Some(1));
        assert_eq!(
            position_of(&rows, &oxpath!("settings", "accounts")),
            Some(0)
        );
        assert_eq!(position_of(&rows, &oxpath!("nonexistent")), None);
    }
}
