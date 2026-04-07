//! State sync: read UiStore via broker, update App fields.
//!
//! Bridge between broker-managed state and App's fields that the
//! draw functions read. Called after every broker write.

use crate::app::{App, InputMode, InsertContext};
use ox_broker::ClientHandle;
use structfs_core_store::{Value, path};

/// Read UiStore state from the broker and sync to App fields.
///
/// Updates: mode, insert_context, active_thread, selected_row,
/// scroll, input, cursor. Returns pending_action if one was set
/// by a command dispatch. Does NOT touch thread_views, search,
/// event channels, or agent state.
pub async fn sync_ui_to_app(client: &ClientHandle, app: &mut App) -> Option<String> {
    // Read all UiStore state in one call
    let state = match client.read(&path!("ui")).await {
        Ok(Some(record)) => match record.as_value() {
            Some(Value::Map(m)) => m.clone(),
            _ => return None,
        },
        _ => return None,
    };

    // Mode + insert context
    let mode_str = state.get("mode").and_then(|v| match v {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    });
    let ctx_str = state.get("insert_context").and_then(|v| match v {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    });
    match mode_str {
        Some("insert") => {
            let ctx = match ctx_str {
                Some("compose") => InsertContext::Compose,
                Some("reply") => InsertContext::Reply,
                Some("search") => InsertContext::Search,
                _ => InsertContext::Compose,
            };
            app.mode = InputMode::Insert(ctx);
        }
        _ => {
            app.mode = InputMode::Normal;
        }
    }

    // Active thread
    app.active_thread = state.get("active_thread").and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    });

    // Selection + scroll
    if let Some(Value::Integer(n)) = state.get("selected_row") {
        app.selected_row = *n as usize;
    }
    if let Some(Value::Integer(n)) = state.get("scroll") {
        app.scroll = *n as u16;
    }

    // Input + cursor
    if let Some(Value::String(s)) = state.get("input") {
        app.input = s.clone();
    }
    if let Some(Value::Integer(n)) = state.get("cursor") {
        app.cursor = *n as usize;
    }

    // Pending action
    state.get("pending_action").and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    })
}
