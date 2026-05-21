//! Horns-driven event loop.
//!
//! While the user is on a horns-owned screen (today: settings) the
//! application state machine sits in `run_horns_settings_loop`. It
//! owns the terminal by value and drives both dispatch and render
//! inline by fetching a `SettingsSnapshot` and calling
//! `horns_core::Dispatcher` / `RendererRegistry` directly.
//!
//! Why inline rather than going through horns-core's
//! `KeyDispatchSubscription` / `RenderSubscription`: every settings
//! renderer / command uses a flat-keyspace `Reader`
//! (`SettingsSnapshot`, a `LocalConfig` populated from the broker
//! by walking known prefixes via async `read_subtree`). The broker's
//! `SubCtx::snapshot` is a different shape — it reads individual
//! paths fine but `data.read(empty)` doesn't return a flat
//! `Value::Map` of all keys, which is what helpers like
//! `child_names_under` and `visible_rows::focus_enumeration` rely
//! on. Building the snapshot per dispatch / per frame here keeps
//! the renderers and commands working without restructuring their
//! Reader expectations.
//!
//! The framework primitives still earn their keep: this loop reuses
//! `horns_core::Dispatcher` (capture/target/bubble walk over the
//! cursor's ancestor chain), the binding registry's specificity
//! resolution, the discrete+handler lookup tiers, and the renderer
//! registry. The broker is the destination for the `Vec<Write>` each
//! command emits — commands write through the broker, broker
//! subscriptions react to those writes for any async/cross-cutting
//! work (catalog fetches, network tests, etc.).

use std::time::Duration;

use crossterm::event::{self, Event};
use horns_core::Dispatcher;
use horns_ratatui::{Theme, render_to_frame};
use ox_broker::{BrokerStore, ClientHandle};
use ox_path::oxpath;
use ratatui::DefaultTerminal;
use structfs_core_store::Path;

use crate::key_chord_canonical::parse_key_str;
use crate::key_encode::encode_key;
use crate::settings::commands::navigation::path_from_value;
use crate::settings::snapshot::fetch_settings_view_state;

/// Outcome of the horns session, communicating back to the top-level
/// state machine which way to pivot.
pub enum HornsExit {
    /// User left settings — back to the legacy loop.
    ToLegacy,
}

/// Run the horns settings loop. Takes the terminal by value; returns
/// it by value when the session ends.
pub async fn run_horns_settings_loop(
    _broker: &BrokerStore,
    client: &ClientHandle,
    mut terminal: DefaultTerminal,
) -> std::io::Result<(HornsExit, DefaultTerminal)> {
    // Build the three framework registries once. Reused every frame
    // (renderers in render, bindings + commands in dispatch). The
    // SettingsHandle the install installed on the broker carries its
    // own copies — those are unused by this loop; they live there
    // for any future broker-mediated reactivity.
    let mut bindings = crate::settings::BindingRegistry::new();
    let mut commands = crate::settings::CommandRegistry::new();
    let mut renderers = crate::settings::RendererRegistry::new();
    crate::settings::bindings::register(&mut bindings);
    crate::settings::commands::register_all(&mut commands);
    crate::settings::renderers::register_all(&mut renderers);
    let theme = Theme::default();
    let dispatcher = Dispatcher::new(crate::settings::cursor_path());

    // Seed the focus cursor if it's not already set. Renderers and
    // dispatch both key off the cursor path; an unset cursor defaults
    // to `settings/index` so the first frame shows the accordion and
    // page-level bindings (j/k/a/...) are reachable.
    use structfs_core_store::Record;
    let cursor_path = crate::settings::cursor_path();
    let cursor_is_set = client.read(&cursor_path).await.ok().flatten().is_some();
    if !cursor_is_set {
        let _ = client
            .write(
                &cursor_path,
                Record::parsed(crate::settings::commands::navigation::path_to_value(
                    &oxpath!("settings", "index"),
                )),
            )
            .await;
    }

    let exit = loop {
        // ---- Render: fetch a snapshot, resolve the page cursor,
        //      run the renderer, draw. The PAGE cursor
        //      (`ui/settings/cursor`) selects which renderer fires;
        //      the FOCUS cursor (`ui/settings/focused`) drives the
        //      dispatcher and the renderer's selection state. They're
        //      distinct concepts — focus can sit on a compound-widget
        //      leaf like `settings/_compose_form/name` while the page
        //      is still `settings/index`.
        let mut snap = fetch_settings_view_state(client).await;
        let cursor = read_page_cursor(&mut snap).unwrap_or_else(|| oxpath!("settings", "index"));
        let area_ratatui = terminal.size()?;
        let area = horns_core::Rect::new(0, 0, area_ratatui.width, area_ratatui.height);
        let view = {
            let mut ctx = crate::settings::RenderCtx {
                area,
                data: &mut snap,
                registry: &renderers,
                theme: &theme as &dyn std::any::Any,
            };
            renderers.render(&cursor, &mut ctx)
        };
        terminal.draw(|frame| {
            let area = frame.area();
            render_to_frame(&view, frame, area, &theme);
        })?;

        // ---- Input: poll briefly for a crossterm event. On a key
        //      event, dispatch INLINE through the framework's
        //      `Dispatcher` against a fresh `SettingsSnapshot`. The
        //      command produces `Vec<Write>` which we forward to the
        //      broker — broker subscriptions (e.g. account-test,
        //      catalog-refresh) react to those writes asynchronously.
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        if let Some(chord) = parse_key_str(&key_str) {
                            let mut snap = fetch_settings_view_state(client).await;
                            let writes = dispatcher
                                .dispatch(&mut snap, &chord, &bindings, &commands, &renderers);
                            for write in writes {
                                let _ = client.write(&write.path, write.record).await;
                            }
                        }
                    }
                }
                Event::Resize(_, _) => {
                    // No-op: the next frame's `terminal.size()` will
                    // pick up the new dimensions automatically.
                }
                _ => {}
            }
        }

        // ---- Exit signal: `_request_exit = true` is written by the
        //      `nav.ascend` command when the user Escapes from the
        //      top-level settings page. Clear it and pivot back to
        //      the legacy loop.
        let exit_path: Path = oxpath!("ui", "settings", "_request_exit");
        let want_exit = client
            .read_typed::<bool>(&exit_path)
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
        if want_exit {
            let _ = client.write_typed(&exit_path, &false).await;
            // Tell UiStore to take us out of Settings screen state
            // *before* the legacy loop's first frame runs, otherwise
            // it would immediately see Screen::Settings and bounce
            // straight back into the horns loop.
            use ox_types::{GlobalCommand, UiCommand};
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::Global(GlobalCommand::GoToInbox))
                .await;
            break HornsExit::ToLegacy;
        }
    };

    Ok((exit, terminal))
}

/// Read the page cursor (`ui/settings/cursor`) — selects which
/// renderer the registry runs. Distinct from the focus cursor at
/// `ui/settings/focused` which selects highlight + dispatch scope.
fn read_page_cursor(snap: &mut crate::settings::snapshot::SettingsSnapshot) -> Option<Path> {
    use structfs_core_store::Reader;
    let rec = snap
        .read(&oxpath!("ui", "settings", "cursor"))
        .ok()
        .flatten()?;
    let value = rec.as_value()?;
    path_from_value(value)
}
