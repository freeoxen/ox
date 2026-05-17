//! Horns-driven event loop.
//!
//! While the user is on a horns-owned screen (today: settings) the
//! application state machine sits in `run_horns_settings_loop`. It
//! owns the terminal by value, installs the ratatui
//! `ViewRenderSubscription` against that terminal under a scoped
//! `Arc<parking_lot::Mutex<...>>` (bounded to this function), polls
//! crossterm input, writes `KeyChord`s to the broker, and watches
//! for the user-driven exit signal. On exit it unwraps the Mutex,
//! recovers the terminal, and returns it to the caller (the
//! application state machine) which transitions back into the legacy
//! loop.
//!
//! The `Arc<Mutex<...>>` is internal — main.rs and `settings::install`
//! never see it. Ownership of the terminal transfers cleanly between
//! states.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event};
use horns_core::subscription::SubscriptionId;
use horns_ratatui::{RatatuiOptions, Theme};
use ox_broker::{BrokerStore, ClientHandle};
use ox_path::oxpath;
use parking_lot::Mutex;
use ratatui::DefaultTerminal;
use structfs_core_store::Path;

use crate::key_chord_canonical::parse_key_str;
use crate::key_encode::encode_key;

/// Outcome of the horns session, communicating back to the top-level
/// state machine which way to pivot.
pub enum HornsExit {
    /// User left settings — back to the legacy loop.
    ToLegacy,
    /// User asked to quit the program from inside settings.
    Quit,
}

/// Run the horns settings loop. Takes the terminal by value; returns
/// it by value when the session ends. The internal subscription
/// machinery is bounded to this function.
pub async fn run_horns_settings_loop(
    broker: &BrokerStore,
    client: &ClientHandle,
    terminal: DefaultTerminal,
) -> std::io::Result<(HornsExit, DefaultTerminal)> {
    // ---- 1. Wrap the terminal in an Arc<Mutex<>> for the duration
    //         of this session. The subscription's `handle` method is
    //         `&self`, so the terminal needs interior mutability. The
    //         Mutex is scoped to this function — nothing outside
    //         touches it.
    let terminal_arc = Arc::new(Mutex::new(terminal));

    // ---- 2. Install the ratatui ViewRenderSubscription. It watches
    //         `<render_output_path>` and locks the terminal to draw
    //         on every new View. horns-core's RenderSubscription is
    //         the producer; this is the consumer.
    let ratatui_handle = horns_ratatui::install(
        broker,
        RatatuiOptions {
            view_input_path: crate::settings::render_output_path(),
            terminal: terminal_arc.clone(),
            theme: Theme::default(),
        },
    );
    let ratatui_sub_id = ratatui_handle.subscription_id.clone();

    // ---- 3. Seed the focus cursor if it's not already set. The
    //         RenderSubscription reads `ui/settings/focused` and
    //         no-ops when the path is empty — without seeding, the
    //         first frame after entering settings would be blank
    //         until the user moved focus. Default to
    //         `settings/index` (the accordion page); j/k will move
    //         focus to the first row immediately.
    use structfs_core_store::{Record, Value};
    let focused_path = crate::settings::cursor_path();
    let focused_is_set = client
        .read(&focused_path)
        .await
        .ok()
        .flatten()
        .is_some();
    if !focused_is_set {
        let _ = client
            .write(
                &focused_path,
                Record::parsed(crate::settings::commands::navigation::path_to_value(
                    &oxpath!("settings", "index"),
                )),
            )
            .await;
    }

    // ---- 4. Seed the area + render-tick so the first frame renders
    //         immediately (the RenderSubscription needs an area to
    //         render against, and a tick to fire on).
    {
        let area = terminal_arc.lock().size()?;
        let area_rect = horns_core::Rect::new(0, 0, area.width, area.height);
        let _ = client
            .write_typed(&crate::settings::input_area_path(), &area_rect)
            .await;
    }
    let _ = client
        .write(
            &crate::settings::render_tick_path(),
            Record::parsed(Value::Integer(1)),
        )
        .await;

    // ---- 4. Input loop. Poll crossterm, encode to KeyChord, write
    //         to the broker's input path. The broker dispatches the
    //         KeyDispatchSubscription which runs the matched command;
    //         the resulting cascade bumps render-tick which wakes the
    //         RenderSubscription; the new View is written to the
    //         render-output path; this loop's ViewRenderSubscription
    //         picks it up and draws.
    let exit = loop {
        // Block briefly for an event; tick periodically to check the
        // exit signal even if no input arrives.
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
                    let area_rect = horns_core::Rect::new(0, 0, w, h);
                    let _ = client
                        .write_typed(&crate::settings::input_area_path(), &area_rect)
                        .await;
                }
                _ => {}
            }
        }

        // Exit signal: when the user presses Esc on the settings
        // index, the `nav.ascend` command writes
        // `ui/settings/_request_exit = true`. Clear it and break out
        // to the legacy loop.
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

    // ---- 5. Tear down: unregister the ratatui subscription and
    //         recover the terminal. The Arc has exactly two strong
    //         references at this point — the local `terminal_arc`
    //         and the subscription's clone. Unregistering drops the
    //         subscription's clone, leaving us as the unique owner.
    broker
        .unregister_subscription(&ratatui_sub_id);
    let terminal = match Arc::try_unwrap(terminal_arc) {
        Ok(mutex) => mutex.into_inner(),
        Err(arc) => {
            // The subscription failed to drop its Arc — should not
            // happen unless ox-broker leaks references somewhere.
            // Fall back to leaking the inner terminal by cloning the
            // mutex's contents is impossible (DefaultTerminal isn't
            // Clone). The only recovery is to return an error.
            tracing::error!(
                strong_count = Arc::strong_count(&arc),
                "horns session: failed to recover Terminal (subscription left dangling refs)"
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "horns session: terminal not uniquely owned at exit",
            ));
        }
    };

    Ok((exit, terminal))
}

/// Identifier of the ratatui subscription registered by
/// `run_horns_settings_loop`. Exposed for tests / introspection.
#[allow(dead_code)]
pub fn ratatui_subscription_id() -> SubscriptionId {
    SubscriptionId("horns_ratatui.view_render".to_string())
}
