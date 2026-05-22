//! Horns-driven event loop.
//!
//! While the user is on a horns-owned screen (today: settings) the
//! application state machine sits in `run_horns_settings_loop`. It
//! owns the terminal by value and feeds inputs to the broker; the
//! framework's `KeyDispatchSubscription`, `RenderSubscription`, and
//! the ratatui backend's `ViewRenderSubscription` do the actual
//! dispatch + render + draw.
//!
//! Inputs are writes; dispatch + render happen in subscriptions. The
//! host loop polls crossterm, encodes each key as a `KeyChord`, writes
//! it to the broker's input path, and watches `_request_exit` to know
//! when to hand the terminal back to the legacy loop.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event};
use ox_broker::{BrokerStore, ClientHandle};
use ox_path::oxpath;
use parking_lot::Mutex;
use ratatui::DefaultTerminal;
use structfs_core_store::{Path, Record, Value};

use crate::key_chord_canonical::parse_key_str;
use crate::key_encode::encode_key;

/// Outcome of the horns session, communicating back to the top-level
/// state machine which way to pivot.
pub enum HornsExit {
    /// User left settings — back to the legacy loop.
    ToLegacy,
}

/// Run the horns settings loop. Takes the terminal by value; returns
/// it by value when the session ends.
pub async fn run_horns_settings_loop(
    broker: &BrokerStore,
    client: &ClientHandle,
    terminal: DefaultTerminal,
) -> std::io::Result<(HornsExit, DefaultTerminal)> {
    // Wrap the terminal for the ratatui subscription's interior
    // mutability. The Arc is scoped to this function — the host
    // doesn't share it elsewhere; the ratatui subscription is the
    // only other lock-holder while horns owns the screen.
    let terminal_arc = Arc::new(Mutex::new(terminal));

    // Install the ratatui backend: ViewRenderSubscription watches the
    // configured view_input_path and draws on every write. Holding the
    // RatatuiHandle keeps the subscription id around so we can
    // unregister at teardown.
    let ratatui_handle = horns_ratatui::install(
        broker,
        horns_ratatui::RatatuiOptions {
            view_input_path: crate::settings::render_output_path(),
            terminal: terminal_arc.clone(),
            theme: horns_ratatui::Theme::default(),
        },
    );

    seed_initial_state(client, &terminal_arc).await?;

    let exit = loop {
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
                Event::Resize(w, h) => {
                    let area = horns_core::Rect::new(0, 0, w, h);
                    let _ = client
                        .write_typed(&crate::settings::input_area_path(), &area)
                        .await;
                }
                _ => {}
            }
        }

        // Exit signal — `nav.ascend` writes this at the index page.
        let exit_path: Path = oxpath!("ui", "settings", "_request_exit");
        let want_exit = client
            .read_typed::<bool>(&exit_path)
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
        if want_exit {
            let _ = client.write_typed(&exit_path, &false).await;
            // Tell UiStore to take us out of the Settings screen state
            // *before* the legacy loop's first frame runs, otherwise it
            // would immediately see Screen::Settings and bounce straight
            // back into the horns loop.
            use ox_types::{GlobalCommand, UiCommand};
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::Global(GlobalCommand::GoToInbox))
                .await;
            break HornsExit::ToLegacy;
        }
    };

    // Teardown: unregister the ratatui subscription so its terminal
    // lock isn't held after this function returns, then recover the
    // terminal from the Arc.
    broker.unregister_subscription(&ratatui_handle.subscription_id);
    let terminal = Arc::try_unwrap(terminal_arc)
        .map_err(|_| std::io::Error::other("horns session: terminal not uniquely owned at exit"))?
        .into_inner();

    Ok((exit, terminal))
}

/// Seed enough state for the first render to fire: focus cursor (so
/// `RenderSubscription` knows which renderer to run), terminal area,
/// and a render-tick bump.
async fn seed_initial_state(
    client: &ClientHandle,
    terminal: &Arc<Mutex<DefaultTerminal>>,
) -> std::io::Result<()> {
    let focus_path = crate::settings::cursor_path();
    let focus_set = client.read(&focus_path).await.ok().flatten().is_some();
    if !focus_set {
        let _ = client
            .write(
                &focus_path,
                Record::parsed(crate::settings::commands::navigation::path_to_value(
                    &oxpath!("settings", "index"),
                )),
            )
            .await;
    }

    let size = terminal.lock().size()?;
    let area = horns_core::Rect::new(0, 0, size.width, size.height);
    let _ = client
        .write_typed(&crate::settings::input_area_path(), &area)
        .await;

    let _ = client
        .write(
            &crate::settings::render_tick_path(),
            Record::parsed(Value::Integer(1)),
        )
        .await;

    Ok(())
}
