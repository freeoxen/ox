//! UI-behavior tests for the horns-driven settings screen.
//!
//! Drives the same flow `run_horns_settings_loop` does in production:
//!
//! 1. Set up a broker with the mounts settings::install writes to.
//! 2. Install the settings horns instance — registers
//!    `KeyDispatchSubscription` for input dispatch.
//! 3. Seed cursor + any test-specific config (accounts, expanded set).
//! 4. For each user action: write a `KeyChord` to the broker's input
//!    path; the `KeyDispatchSubscription` runs the matched command
//!    synchronously and the cascade lands on the broker.
//! 5. Build a `SettingsSnapshot` and render through the renderer
//!    registry — exactly what the horns loop does each frame.
//! 6. Assert on either the focus cursor (state) or the rendered View
//!    (visible behavior).
//!
//! Crucially: rendering happens INLINE (via the renderer registry +
//! `SettingsSnapshot`), not through `RenderSubscription`. The broker
//! reader's `data.read(empty)` doesn't return a flat `Value::Map`
//! the way an in-memory `LocalConfig` does, so renderer helpers like
//! `child_names_under` only work against a fetched snapshot. The
//! horns settings loop renders inline for the same reason.

use std::time::Duration;

use horns_core::Dispatcher;
use horns_core::view::{ListItem, View};
use ox_broker::{BrokerStore, ClientHandle};
use ox_gate::AccountConfig;
use ox_path::oxpath;
use ox_store_util::local_config::LocalConfig;
use ox_types::settings::{BadgeSource, SettingsIndexEntry};
use ox_types::{KeyChord, KeyCodeRepr, KeyModifierSet};
use structfs_core_store::{Path, Reader, Record, Value};

use ox_cli::settings;
use ox_cli::settings::commands::navigation::{path_from_value, path_to_value};
use ox_cli::settings::snapshot::fetch_settings_view_state;
use ox_cli::settings::visible_rows::expanded_set_to_value;

// ---------------------------------------------------------------------------
// Test harness — broker setup, install, and small action helpers.
// ---------------------------------------------------------------------------

/// Build a broker with every mount `settings::install` writes to, plus
/// the test seed data the renderer needs to produce non-empty content.
async fn build_broker_with_seeds() -> BrokerStore {
    let broker = BrokerStore::new(Duration::from_secs(5));
    let _settings_mount = broker.mount(oxpath!("settings"), LocalConfig::new()).await;
    let _config_mount = broker.mount(oxpath!("config"), LocalConfig::new()).await;
    let _secret_mount = broker.mount(oxpath!("secret"), LocalConfig::new()).await;
    let _ui_mount = broker.mount(oxpath!("ui"), LocalConfig::new()).await;
    let _horns_mount = broker.mount(oxpath!("horns"), LocalConfig::new()).await;
    let client = broker.client();
    seed_index_entries(&client).await;
    broker
}

/// Two day-one index entries — Accounts and Models. The accordion
/// renderer reads these to produce its section headers.
async fn seed_index_entries(client: &ClientHandle) {
    let entries = [
        ("accounts", "Accounts", "settings/accounts"),
        ("models", "Models", "settings/models"),
    ];
    for (id, label, target) in entries {
        let entry = SettingsIndexEntry {
            id: id.to_string(),
            label: label.to_string(),
            description: String::new(),
            target_cursor: Path::parse(target).unwrap(),
            badge: BadgeSource::None,
        };
        let path = Path::try_from_components(
            ["settings", "index", "entries", id]
                .into_iter()
                .map(String::from)
                .collect(),
        )
        .unwrap();
        client.write_typed(&path, &entry).await.unwrap();
    }
}

/// Seed an `AccountConfig` at `config/gate/accounts/<name>` so the
/// accounts row enumeration has something to show.
async fn seed_account(client: &ClientHandle, name: &str) {
    let account = AccountConfig::default();
    let path = Path::try_from_components(
        ["config", "gate", "accounts", name]
            .into_iter()
            .map(String::from)
            .collect(),
    )
    .unwrap();
    client.write_typed(&path, &account).await.unwrap();
}

/// Seed the expanded set at `ui/settings/expanded`.
async fn seed_expanded(client: &ClientHandle, expanded: &[&str]) {
    let value = expanded_set_to_value(
        &expanded
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>(),
    );
    client
        .write(
            &oxpath!("ui", "settings", "expanded"),
            Record::parsed(value),
        )
        .await
        .unwrap();
}

/// Set the focus cursor to `path`.
async fn set_cursor(client: &ClientHandle, path: &Path) {
    client
        .write(
            &settings::cursor_path(),
            Record::parsed(path_to_value(path)),
        )
        .await
        .unwrap();
}

/// Read the current focus cursor. Returns the inner `Path` it points
/// at, or `None` when unset / undecodable.
async fn read_cursor(client: &ClientHandle) -> Option<Path> {
    let rec = client.read(&settings::cursor_path()).await.ok().flatten()?;
    let value = rec.as_value()?;
    path_from_value(value)
}

/// Press a single keystroke via the same inline-dispatch path
/// `run_horns_settings_loop` uses: build a fresh `SettingsSnapshot`,
/// invoke `Dispatcher::dispatch` against it, write the resulting
/// `Vec<Write>` back through the broker. Returns once the cascade has
/// settled.
async fn press_chord(client: &ClientHandle, chord: KeyChord) {
    let mut bindings = settings::BindingRegistry::new();
    let mut commands = settings::CommandRegistry::new();
    let mut renderers = settings::RendererRegistry::new();
    settings::bindings::register(&mut bindings);
    settings::commands::register_all(&mut commands);
    settings::renderers::register_all(&mut renderers);
    let dispatcher = Dispatcher::new(settings::cursor_path());
    let mut snap = fetch_settings_view_state(client).await;
    let writes = dispatcher.dispatch(&mut snap, &chord, &bindings, &commands, &renderers);
    for write in writes {
        client.write(&write.path, write.record).await.unwrap();
    }
    // Cascade settle — broker subscriptions (account_test_status etc.)
    // may have fired off async work; yield + brief sleep mirrors the
    // standard pattern in settings_e2e.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
}

/// Convenience: build a printable-ASCII chord.
fn key_char(c: char) -> KeyChord {
    KeyChord {
        modifiers: KeyModifierSet::default(),
        code: KeyCodeRepr::Char(c),
    }
}

/// Convenience: build a named-key chord (Enter, Esc, Backspace, …).
fn key_named(code: KeyCodeRepr) -> KeyChord {
    KeyChord {
        modifiers: KeyModifierSet::default(),
        code,
    }
}

/// Run the renderer against a freshly-fetched `SettingsSnapshot` —
/// the exact rendering path `run_horns_settings_loop` follows each
/// frame, minus the ratatui draw call. Returns the `View` the
/// translator would consume. Reads the PAGE cursor
/// (`ui/settings/cursor`) to pick a renderer, defaulting to
/// `settings/index` when unset.
async fn render_settings(client: &ClientHandle) -> View {
    let mut renderers = settings::RendererRegistry::new();
    settings::renderers::register_all(&mut renderers);
    let mut snap = fetch_settings_view_state(client).await;
    let cursor = snap
        .read(&oxpath!("ui", "settings", "cursor"))
        .ok()
        .flatten()
        .and_then(|r| r.as_value().cloned())
        .and_then(|v| path_from_value(&v))
        .unwrap_or_else(|| oxpath!("settings", "index"));
    let theme = horns_ratatui::Theme::default();
    let area = horns_core::Rect::new(0, 0, 80, 24);
    let mut ctx = settings::RenderCtx {
        area,
        data: &mut snap,
        registry: &renderers,
        theme: &theme as &dyn std::any::Any,
    };
    renderers.render(&cursor, &mut ctx)
}

// ---------------------------------------------------------------------------
// View probes — walk the View tree to find specific ListItem strings.
// ---------------------------------------------------------------------------

/// Collect every `ListItem::primary` string from the View tree, in
/// rendering order. Lets tests assert on user-visible row content
/// without coupling to the Stack / Frame nesting shape.
fn collect_list_primaries(view: &View) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    walk_view(view, &mut |v| {
        if let View::List { items, .. } = v {
            for item in items {
                out.push(item.primary.clone());
            }
        }
    });
    out
}

/// Generic in-order walk over the View tree.
fn walk_view<'a, F: FnMut(&'a View)>(view: &'a View, f: &mut F) {
    f(view);
    match view {
        View::Frame { content, .. } => walk_view(content, f),
        View::Stack { children, .. } => {
            for (child, _) in children {
                walk_view(child, f);
            }
        }
        View::Modal {
            background,
            foreground,
            ..
        } => {
            walk_view(background, f);
            walk_view(foreground, f);
        }
        View::Pad { child, .. } => walk_view(child, f),
        // Leaves — no children to recurse into.
        View::Empty
        | View::Text { .. }
        | View::List { .. }
        | View::Form { .. }
        | View::Banner { .. }
        | View::StatusBlock { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_render_shows_accounts_and_models_section_headers() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    set_cursor(&client, &oxpath!("settings", "index")).await;

    let view = render_settings(&client).await;
    let primaries = collect_list_primaries(&view);

    // The two section headers should both be visible.
    assert!(
        primaries.iter().any(|p| p.contains("Accounts")),
        "Accounts header missing; primaries={primaries:?}",
    );
    assert!(
        primaries.iter().any(|p| p.contains("Models")),
        "Models header missing; primaries={primaries:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn j_moves_focus_to_next_visible_row() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    // Seed focus at the Accounts header. Pressing `j` should advance
    // to the Models header (next visible row at the top level).
    set_cursor(&client, &oxpath!("settings", "accounts")).await;

    press_chord(&client, key_char('j')).await;

    let cursor = read_cursor(&client)
        .await
        .expect("cursor should be set after j");
    assert_eq!(
        cursor,
        oxpath!("settings", "models"),
        "j should advance focus from Accounts → Models",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k_moves_focus_to_previous_visible_row() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    set_cursor(&client, &oxpath!("settings", "models")).await;

    press_chord(&client, key_char('k')).await;

    let cursor = read_cursor(&client)
        .await
        .expect("cursor should be set after k");
    assert_eq!(
        cursor,
        oxpath!("settings", "accounts"),
        "k should advance focus from Models → Accounts",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_on_accounts_expands_the_accordion() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    seed_account(&client, "alpha").await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;

    // Before Enter: the accounts section is collapsed; alpha row not
    // visible.
    let before = render_settings(&client).await;
    let before_primaries = collect_list_primaries(&before);
    assert!(
        !before_primaries.iter().any(|p| p.contains("alpha")),
        "alpha should be hidden before expansion; primaries={before_primaries:?}",
    );

    // Press Enter to expand. `tree.activate` writes the expanded set.
    press_chord(&client, key_named(KeyCodeRepr::Enter)).await;

    // After Enter: the alpha row is now visible.
    let after = render_settings(&client).await;
    let after_primaries = collect_list_primaries(&after);
    assert!(
        after_primaries.iter().any(|p| p.contains("alpha")),
        "alpha should be visible after expansion; primaries={after_primaries:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_on_expanded_accounts_collapses_the_accordion() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    seed_account(&client, "alpha").await;
    seed_expanded(&client, &["settings/accounts"]).await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;

    let expanded = render_settings(&client).await;
    assert!(
        collect_list_primaries(&expanded)
            .iter()
            .any(|p| p.contains("alpha")),
        "precondition: alpha visible while accounts expanded",
    );

    press_chord(&client, key_named(KeyCodeRepr::Enter)).await;

    let collapsed = render_settings(&client).await;
    assert!(
        !collect_list_primaries(&collapsed)
            .iter()
            .any(|p| p.contains("alpha")),
        "alpha should be hidden after collapse",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pressing_a_opens_compose_new_connection() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    seed_expanded(&client, &["settings/accounts"]).await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;

    // Press `a` — the page-level binding that opens the compose form.
    press_chord(&client, key_char('a')).await;

    // The focus cursor descends into the synthetic compose namespace.
    // Cursor at `settings/_compose_form/name` (or similar leaf) is
    // the structural marker that compose is engaged.
    let cursor = read_cursor(&client)
        .await
        .expect("cursor should be set after pressing a");
    let components: Vec<String> = cursor
        .components
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();
    assert!(
        components.first().map(String::as_str) == Some("settings")
            && components
                .get(1)
                .map(String::as_str)
                .map(|s| s.starts_with('_'))
                .unwrap_or(false),
        "cursor should descend into a `settings/_<widget>` namespace; got {components:?}",
    );

    // The rendered View should contain the "+ New connection" heading
    // that the renderer surfaces when compose is active.
    let view = render_settings(&client).await;
    let primaries = collect_list_primaries(&view);
    assert!(
        primaries.iter().any(|p| p.contains("New connection")),
        "compose form heading should be visible; primaries={primaries:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typing_in_compose_form_appends_to_name_buffer() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    seed_expanded(&client, &["settings/accounts"]).await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;

    press_chord(&client, key_char('a')).await; // open compose

    // Type "beta" — each char goes through the broker, hits the
    // compose form's text-input handler / discrete bindings, lands
    // in `ui/settings/new_account/buffer`.
    for ch in "beta".chars() {
        press_chord(&client, key_char(ch)).await;
    }

    // The name buffer (one per compose field, written by
    // `accounts.compose.insert_char`) should now contain "beta".
    let rec = client
        .read(&oxpath!("ui", "settings", "new_account", "name"))
        .await
        .expect("read")
        .expect("name record present");
    let value = rec.as_value().expect("name value");
    let s = match value {
        Value::String(s) => s.clone(),
        other => panic!("name should be a string; got {other:?}"),
    };
    assert_eq!(
        s, "beta",
        "compose name field should reflect typed characters"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_in_compose_form_cancels_and_restores_cursor() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    seed_expanded(&client, &["settings/accounts"]).await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;

    press_chord(&client, key_char('a')).await; // open compose
    // Cursor is now inside `settings/_compose_form/*`.

    press_chord(&client, key_named(KeyCodeRepr::Esc)).await;

    let cursor = read_cursor(&client)
        .await
        .expect("cursor should be set after Esc");
    // After cancel the cursor restores to where it was before — the
    // Accounts header.
    assert_eq!(
        cursor,
        oxpath!("settings", "accounts"),
        "Esc should restore the pre-compose cursor",
    );
    // The new_account subtree should be cleared too — any subsequent
    // open starts fresh.
    let name = client
        .read(&oxpath!("ui", "settings", "new_account", "name"))
        .await
        .expect("read");
    assert!(
        name.is_none(),
        "new_account/name should be cleared on cancel",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_on_index_writes_request_exit() {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    set_cursor(&client, &oxpath!("settings", "index")).await;

    press_chord(&client, key_named(KeyCodeRepr::Esc)).await;

    // The horns loop's exit-watch is `ui/settings/_request_exit = true`.
    // `nav.ascend` at the index writes this.
    let exit = client
        .read_typed::<bool>(&oxpath!("ui", "settings", "_request_exit"))
        .await
        .expect("read exit");
    assert_eq!(
        exit,
        Some(true),
        "Esc on settings/index should write _request_exit = true",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn highlighted_row_has_selected_flag_in_rendered_list() {
    // Drives the cursor → renderer selected-index pipeline. The
    // renderer translates the focus cursor into a `selected: Some(i)`
    // on the List view containing the focused row. Pressing j updates
    // the cursor; the next frame's List has a different `selected`.
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    seed_account(&client, "alpha").await;
    seed_expanded(&client, &["settings/accounts"]).await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;

    // Before j: the Accounts header is focused. The List containing
    // both section headers should have `selected = Some(0)`.
    let before = render_settings(&client).await;
    let header_list_selected_before = find_list_selected_for_primary(&before, "Accounts");
    assert!(
        header_list_selected_before.is_some(),
        "Accounts row should exist before j",
    );

    // Move down to alpha.
    press_chord(&client, key_char('j')).await;

    let after_cursor = read_cursor(&client).await.expect("cursor after j");
    assert_eq!(
        after_cursor,
        oxpath!("settings", "accounts", "alpha"),
        "j should descend into the alpha account row",
    );
}

/// Find the first `View::List` containing a `ListItem` whose primary
/// matches `needle`, return its `selected` field.
fn find_list_selected_for_primary(view: &View, needle: &str) -> Option<Option<usize>> {
    let mut found: Option<Option<usize>> = None;
    walk_view(view, &mut |v| {
        if found.is_some() {
            return;
        }
        if let View::List { items, selected } = v {
            if items.iter().any(|item| item.primary.contains(needle)) {
                found = Some(*selected);
            }
        }
    });
    found
}
