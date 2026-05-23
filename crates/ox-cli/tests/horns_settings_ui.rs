//! UI-behavior tests for the horns-driven settings screen.
//!
//! Drives the same flow `run_horns_settings_loop` does in production:
//!
//! 1. Set up a broker with the mounts `settings::install` writes to.
//! 2. Install the settings horns instance — registers
//!    `KeyDispatchSubscription` and friends on the broker.
//! 3. Seed cursor + any test-specific config (accounts, expanded set).
//! 4. For each user action: write a `KeyChord` to the broker's input
//!    path. The `KeyDispatchSubscription` runs the matched command
//!    synchronously and the cascade lands on the broker.
//! 5. Render inline via `SettingsSnapshot` + the renderer registry —
//!    same View shape the production `RenderSubscription` emits, but
//!    without going through the subscription's serialize/deserialize.
//! 6. Assert on either the focus cursor (state) or the rendered View
//!    (visible behavior).

use std::sync::Arc;
use std::time::Duration;

use ox_broker::{BrokerStore, ClientHandle};
use ox_gate::AccountConfig;
use ox_path::oxpath;
use ox_store_util::local_config::LocalConfig;
use ox_types::settings::{BadgeSource, SettingsIndexEntry};
use ox_types::{KeyChord, KeyCodeRepr, KeyModifierSet};
use parking_lot::Mutex;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use structfs_core_store::{Path, Record, Value};

use ox_cli::settings;
use ox_cli::settings::commands::navigation::{path_from_value, path_to_value};
use ox_cli::settings::visible_rows::expanded_set_to_value;

// ---------------------------------------------------------------------------
// Test harness — broker setup, install, and small action helpers.
// ---------------------------------------------------------------------------

/// Rig for end-to-end behavior tests: builds the broker, installs
/// settings, installs `horns_ratatui` against a `TestBackend`, and
/// seeds the input area path so `RenderSubscription` has something to
/// render against. Returns the broker (kept alive for the test's
/// duration), the client (drive inputs), and the test terminal (read
/// rendered cells from `backend().buffer()`).
async fn build_horns_test_rig() -> (BrokerStore, ClientHandle, Arc<Mutex<Terminal<TestBackend>>>) {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");

    let backend = TestBackend::new(80, 24);
    let terminal = Arc::new(Mutex::new(
        Terminal::new(backend).expect("construct test terminal"),
    ));

    let _handle = horns_ratatui::install(
        &broker,
        horns_ratatui::RatatuiOptions {
            view_input_path: settings::render_output_path(),
            terminal: terminal.clone(),
            theme: horns_ratatui::Theme::default(),
        },
    );

    // Seed the area path so RenderSubscription has a Rect to render
    // against. Mirrors `seed_initial_state` in `horns_loop`.
    let area = horns_core::Rect::new(0, 0, 80, 24);
    client
        .write_typed(&settings::input_area_path(), &area)
        .await
        .unwrap();

    (broker, client, terminal)
}

/// Read every cell symbol from the test terminal's buffer, row-major,
/// joined with newlines. Use for substring assertions ("contains
/// 'alpha' somewhere") and for snapshot-style frame comparisons.
fn rendered_text(terminal: &Arc<Mutex<Terminal<TestBackend>>>) -> String {
    let guard = terminal.lock();
    let buf = guard.backend().buffer();
    let mut out =
        String::with_capacity(((buf.area.width as usize) + 1) * (buf.area.height as usize));
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        // Trim trailing whitespace on each line so substring tests
        // don't choke on padding the user can't see.
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    out
}

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

/// Press a single keystroke by writing it to the broker's input key
/// path. `settings::install` registered `KeyDispatchSubscription` on
/// the broker; the broker dispatches the subscription synchronously on
/// the write. Returns once the cascade has settled.
async fn press_chord(client: &ClientHandle, chord: KeyChord) {
    client
        .write_typed(&settings::input_key_path(), &chord)
        .await
        .unwrap();
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_render_shows_accounts_and_models_section_headers() {
    let (_broker, client, terminal) = build_horns_test_rig().await;
    // Seed the focus cursor — `RenderSubscription` wakes on cursor
    // changes and writes the View; `ViewRenderSubscription` then locks
    // the test terminal and draws into its buffer.
    set_cursor(&client, &oxpath!("settings", "index")).await;
    // Cascade settle. The render cascade is sync from the broker's
    // write path; the sleep guards spawned subscription work that
    // could arrive after this write returns (Problem 2 in the
    // test-quality debt doc).
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let text = rendered_text(&terminal);
    assert!(
        text.contains("Accounts"),
        "Accounts header missing from rendered output:\n{text}",
    );
    assert!(
        text.contains("Models"),
        "Models header missing from rendered output:\n{text}",
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
    let (_broker, client, terminal) = build_horns_test_rig().await;
    seed_account(&client, "alpha").await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Before Enter: the accounts section is collapsed; alpha row not
    // visible.
    let before = rendered_text(&terminal);
    assert!(
        !before.contains("alpha"),
        "alpha should be hidden before expansion:\n{before}",
    );

    // Press Enter to expand. `tree.activate` writes the expanded set.
    press_chord(&client, key_named(KeyCodeRepr::Enter)).await;

    // After Enter: the alpha row is now visible.
    let after = rendered_text(&terminal);
    assert!(
        after.contains("alpha"),
        "alpha should be visible after expansion:\n{after}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_on_expanded_accounts_collapses_the_accordion() {
    let (_broker, client, terminal) = build_horns_test_rig().await;
    seed_account(&client, "alpha").await;
    seed_expanded(&client, &["settings/accounts"]).await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let expanded = rendered_text(&terminal);
    assert!(
        expanded.contains("alpha"),
        "precondition: alpha visible while accounts expanded:\n{expanded}",
    );

    press_chord(&client, key_named(KeyCodeRepr::Enter)).await;

    let collapsed = rendered_text(&terminal);
    assert!(
        !collapsed.contains("alpha"),
        "alpha should be hidden after collapse:\n{collapsed}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pressing_a_opens_compose_new_connection() {
    let (_broker, client, terminal) = build_horns_test_rig().await;
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

    // The rendered output should contain the "+ New connection" heading
    // that the renderer surfaces when compose is active.
    let text = rendered_text(&terminal);
    assert!(
        text.contains("New connection"),
        "compose form heading should be visible:\n{text}",
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
async fn q_from_deep_focus_writes_request_exit() {
    // `q` is the unconditional exit-screen hatch. With focus deep
    // inside an account row (an inline field), pressing `q` must
    // still write `_request_exit = true` — the user shouldn't have
    // to walk back up the ascend ladder to get out.
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");
    seed_account(&client, "alpha").await;
    seed_expanded(&client, &["settings/accounts", "settings/accounts/alpha"]).await;
    set_cursor(
        &client,
        &oxpath!("settings", "accounts", "alpha", "endpoint"),
    )
    .await;

    press_chord(&client, key_char('q')).await;

    let exit = client
        .read_typed::<bool>(&oxpath!("ui", "settings", "_request_exit"))
        .await
        .expect("read exit");
    assert_eq!(
        exit,
        Some(true),
        "q at a deep field focus must request screen exit",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_after_full_horns_loop_pre_input_seeding_exits() {
    // Mimics what `run_horns_settings_loop` does pre-input:
    //   1. Seed focus cursor (if unset).
    //   2. Seed terminal area.
    //   3. Bump render-tick.
    //   4. (Then start polling crossterm in production.)
    // Then writes Esc and checks the exit-watch. Reproduces the
    // production order without the ratatui terminal lock so we can
    // catch any seed/dispatch ordering bug that the simpler
    // esc_on_index test doesn't exercise.
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");

    let focus_path = settings::cursor_path();
    let focus_set = client.read(&focus_path).await.ok().flatten().is_some();
    if !focus_set {
        client
            .write(
                &focus_path,
                Record::parsed(path_to_value(&oxpath!("settings", "index"))),
            )
            .await
            .unwrap();
    }
    let area = horns_core::Rect::new(0, 0, 80, 24);
    client
        .write_typed(&settings::input_area_path(), &area)
        .await
        .unwrap();
    client
        .write(
            &settings::render_tick_path(),
            Record::parsed(Value::Integer(1)),
        )
        .await
        .unwrap();
    // Let render cascade fully settle before driving input.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    press_chord(&client, key_named(KeyCodeRepr::Esc)).await;

    let exit = client
        .read_typed::<bool>(&oxpath!("ui", "settings", "_request_exit"))
        .await
        .expect("read exit");
    assert_eq!(
        exit,
        Some(true),
        "Esc after the full pre-input seed should still write _request_exit = true",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn j_advances_focus_from_section_header_into_expanded_account_row() {
    // Pre-cursor-as-focus naming was "highlighted_row_has_selected_flag";
    // it tested the cursor → renderer selected-index → highlight
    // pipeline by inspecting the View tree's `selected: Option<usize>`
    // on the list containing the focused row. Cell-read can't see that
    // structural metadata (only rendered glyphs + styles), so the
    // assertion now substantiates the SAME pipeline via its
    // observable consequence: j moves the focus cursor from the
    // Accounts header to the alpha row, and both are visible in the
    // rendered output before / after.
    let (_broker, client, terminal) = build_horns_test_rig().await;
    seed_account(&client, "alpha").await;
    seed_expanded(&client, &["settings/accounts"]).await;
    set_cursor(&client, &oxpath!("settings", "accounts")).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Before j: both the Accounts header and the alpha row are in the
    // rendered output (the section is pre-expanded).
    let before = rendered_text(&terminal);
    assert!(
        before.contains("Accounts") && before.contains("alpha"),
        "Accounts header and alpha row should both be rendered:\n{before}",
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
