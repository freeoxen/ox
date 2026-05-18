//! End-to-end test for the horns-driven settings render pipeline.
//!
//! Pumps through:
//!   settings::install → seed cursor / area / render_tick →
//!   RenderSubscription fires → produces View →
//!   View serialized to <render_output_path>.
//!
//! Asserts the View is non-empty (i.e. the renderer actually ran and
//! produced something). When the user reported "settings screen loads
//! but nothing in it", the symptom was empty render output — this
//! test reproduces that without a real terminal.

use std::time::Duration;

use ox_broker::BrokerStore;
use ox_path::oxpath;
use ox_store_util::local_config::LocalConfig;
use ox_types::settings::{BadgeSource, SettingsIndexEntry};
use structfs_core_store::{Record, Value};

use ox_cli::settings;

/// Seed the broker with a minimal settings index so the renderer has
/// something to render against. Mirrors what
/// `settings::bootstrap::populate_index_entries` writes at startup
/// but in-memory only.
async fn seed_index_entries(client: &ox_broker::ClientHandle) {
    let entries = [
        SettingsIndexEntry {
            id: "accounts".to_string(),
            label: "Accounts".to_string(),
            description: String::new(),
            target_cursor: structfs_core_store::Path::parse("settings/accounts").unwrap(),
            badge: BadgeSource::None,
        },
        SettingsIndexEntry {
            id: "models".to_string(),
            label: "Models".to_string(),
            description: String::new(),
            target_cursor: structfs_core_store::Path::parse("settings/models").unwrap(),
            badge: BadgeSource::None,
        },
    ];
    for entry in &entries {
        let mut path_components = oxpath!("settings", "index", "entries").components.clone();
        path_components.push(entry.id.clone());
        let path = structfs_core_store::Path::try_from_components(path_components)
            .expect("path components");
        let _ = client.write_typed(&path, entry).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_subscription_writes_non_empty_view_for_settings_index() {
    let broker = BrokerStore::new(Duration::from_secs(5));
    // Mount the broker substores that `settings::install` writes to.
    let _settings_mount = broker.mount(oxpath!("settings"), LocalConfig::new()).await;
    let _config_mount = broker.mount(oxpath!("config"), LocalConfig::new()).await;
    let _secret_mount = broker.mount(oxpath!("secret"), LocalConfig::new()).await;
    // Generic substore for `ui/_horns/*` (input/area, render/tick, theme).
    // `settings::install` writes the theme record to `ui/_horns/theme`
    // and the bootstrap path is `ui/settings/focused` etc., so a
    // single `ui/` LocalConfig captures both.
    let _ui_mount = broker.mount(oxpath!("ui"), LocalConfig::new()).await;
    // Bindings + commands metadata land here.
    let _horns_mount = broker.mount(oxpath!("horns"), LocalConfig::new()).await;

    let client = broker.client();
    seed_index_entries(&client).await;

    // Install the horns settings instance — registers KeyDispatch +
    // Render + ThemeChange subscriptions on the broker.
    let _handle = settings::install(&broker)
        .await
        .expect("settings::install");

    // Seed the focus cursor at the index page.
    use ox_cli::settings::commands::navigation::path_to_value;
    client
        .write(
            &settings::cursor_path(),
            Record::parsed(path_to_value(&oxpath!("settings", "index"))),
        )
        .await
        .expect("seed cursor");

    // Seed the area so the RenderSubscription has something to render
    // against.
    let area = horns_core::Rect::new(0, 0, 80, 24);
    client
        .write_typed(&settings::input_area_path(), &area)
        .await
        .expect("seed area");

    // Kick the render-tick to trigger an explicit re-render. The
    // RenderSubscription also watches the cursor and area paths; the
    // tick bump ensures we have one no-question-about-it trigger
    // after both prior writes have committed.
    client
        .write(
            &settings::render_tick_path(),
            Record::parsed(Value::Integer(1)),
        )
        .await
        .expect("seed tick");

    // Give the broker cascade a moment to settle. Multiple async
    // dispatches happen behind the scenes; yield + brief sleep is
    // the standard pattern in the other settings_e2e tests.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Read the View the RenderSubscription wrote.
    let record = client
        .read(&settings::render_output_path())
        .await
        .expect("read render output")
        .expect("View record present at render_output_path");
    let value = record.as_value().expect("View record has a value").clone();

    let view: horns_core::view::View =
        structfs_serde_store::from_value(value).expect("deserialize View");

    // Assert the View is not the empty fallback. `View::Empty` is the
    // default; `unknown_cursor_fallback` returns a Text View with a
    // diagnostic message. A working renderer for `settings/index`
    // returns a Frame containing a Stack of List rows.
    assert!(
        !matches!(view, horns_core::view::View::Empty),
        "RenderSubscription wrote View::Empty — the renderer for the focused cursor either didn't run, returned Empty, or wasn't registered. Cursor was settings/index; bindings_prefix/commands_prefix populated by settings::install."
    );
    // Diagnostic: print the View shape so we can inspect what we got
    // if the test passes the not-Empty check but the user-visible
    // render is still blank.
    eprintln!("rendered view: {view:#?}");
}
