//! Inbox screen key handling — extracted from event_loop.rs.

use crate::shell::Outcome;
use crossterm::event::KeyCode;
use ox_path::oxpath;
use ox_types::{InboxCommand, UiCommand};

/// Handle inbox-specific keys (normal mode).
///
/// Currently handles search chip dismissal (digit keys 1-9 when search is active).
pub(crate) async fn handle_key(
    key_code: KeyCode,
    search_active: bool,
    client: &ox_broker::ClientHandle,
) -> Outcome {
    // Search chip dismissal (1-9 in normal mode, inbox, search active)
    if search_active {
        if let KeyCode::Char(c @ '1'..='9') = key_code {
            let idx = (c as u8 - b'1') as usize;
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::Inbox(InboxCommand::SearchDismissChip { index: idx }),
                )
                .await;
            return Outcome::Handled;
        }
    }

    Outcome::Ignored
}
