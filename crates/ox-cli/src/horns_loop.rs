//! Horns-driven event loop.
//!
//! While the user is on a horns-owned screen (today: settings) the
//! application state machine sits in `run_horns_settings_loop`. It
//! owns the terminal by value, drives input through the broker (so
//! `KeyDispatchSubscription` handles dispatch with all its cascade
//! semantics), and renders inline by fetching a `SettingsSnapshot`
//! and calling the renderer directly.
//!
//! Why inline render rather than going through horns-core's
//! `RenderSubscription`: the settings renderers use a flat-keyspace
//! `Reader` (`SettingsSnapshot`, a `LocalConfig` populated from the
//! broker by walking known prefixes). The broker's `SubCtx::snapshot`
//! is a different shape — it reads individual paths fine but
//! `data.read(empty)` doesn't return a flat `Value::Map` of all keys,
//! which is what helpers like `child_names_under` rely on. Building
//! the snapshot per frame here keeps the renderer working without
//! restructuring the snapshot interface.
//!
//! Dispatch IS still broker-mediated: keystrokes flow into
//! `ui/_horns/settings/input/key`, the `KeyDispatchSubscription`
//! resolves them through the registered Commands, the resulting
//! cascade writes back to the broker. The next frame fetches a fresh
//! snapshot that reflects those writes.

use std::time::Duration;

use crossterm::event::{self, Event};
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
    /// User asked to quit the program from inside settings.
    Quit,
}

/// Run the horns settings loop. Takes the terminal by value; returns
/// it by value when the session ends.
pub async fn run_horns_settings_loop(
    _broker: &BrokerStore,
    client: &ClientHandle,
    mut terminal: DefaultTerminal,
) -> std::io::Result<(HornsExit, DefaultTerminal)> {
    // Renderers + theme live for the duration of the session. The
    // RendererRegistry is reused every frame; constructing it once
    // is cheap and avoids re-registering closures on each draw.
    let mut renderers = crate::settings::RendererRegistry::new();
    crate::settings::renderers::register_all(&mut renderers);
    let theme = Theme::default();

    // Seed the focus cursor if it's not already set. Renderers key
    // off the cursor path; an unset cursor defaults to
    // `settings/index` so the first frame shows the accordion.
    use structfs_core_store::Record;
    let cursor_path = crate::settings::cursor_path();
    let cursor_is_set = client
        .read(&cursor_path)
        .await
        .ok()
        .flatten()
        .is_some();
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
        // ---- Render: fetch a snapshot, resolve the focus cursor,
        //      run the renderer, draw. The snapshot is built once
        //      per frame so reads see writes that landed since the
        //      last frame (in particular, the cursor / focus updates
        //      the KeyDispatchSubscription emitted).
        let mut snap = fetch_settings_view_state(client).await;
        let cursor = read_focus_cursor(&mut snap)
            .unwrap_or_else(|| oxpath!("settings", "index"));
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

        // ---- Input: poll briefly for a crossterm event; if one
        //      arrives, write it through the broker so
        //      KeyDispatchSubscription handles dispatch (cascade
        //      semantics, command resolution, etc.). Resizes update
        //      the area record for any host that watches it.
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        if let Some(chord) = parse_key_str(&key_str) {
                            let _ = client
                                .write_typed(&crate::settings::input_key_path(), &chord)
                                .await;
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

/// Read the focus cursor (`ui/settings/focused`) from a fetched
/// `SettingsSnapshot`. Returns `None` if the path is empty or
/// missing; the renderer then falls back to a default cursor.
fn read_focus_cursor(snap: &mut crate::settings::snapshot::SettingsSnapshot) -> Option<Path> {
    use structfs_core_store::Reader;
    let rec = snap
        .read(&oxpath!("ui", "settings", "focused"))
        .ok()
        .flatten()?;
    let value = rec.as_value()?;
    path_from_value(value)
}
