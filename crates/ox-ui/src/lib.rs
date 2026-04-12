//! UI stores for ox — state machines driven by StructFS command protocol.
//!
//! Stores are synchronous Reader/Writer implementations. Reads return
//! current state. Writes are commands validated by the command protocol
//! (preconditions + txn deduplication).

pub mod approval_store;
pub mod command;
pub mod command_def;
pub mod config_store;
pub mod input_store;
pub mod text_input_store;
pub mod ui_store;

pub use approval_store::ApprovalStore;
pub use command::{Command, TxnLog};
pub use command_def::{CommandDef, CommandError, CommandInvocation, ParamDef, ParamKind};
pub use config_store::ConfigStore;
pub use input_store::{Action, ActionField, Binding, BindingContext, InputStore};
pub use text_input_store::TextInputStore;
pub use ui_store::UiStore;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, Writer, path};

    /// End-to-end: key event → InputStore → BrokerStore → UiStore state change.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn key_event_through_broker_changes_ui_state() {
        let broker = ox_broker::BrokerStore::default();

        // Mount UiStore
        let mut ui = UiStore::new();
        // Pre-set row_count so selection can advance
        ui.write(
            &path!("set_row_count"),
            Record::parsed(Value::Map({
                let mut m = BTreeMap::new();
                m.insert("count".to_string(), Value::Integer(10));
                m
            })),
        )
        .unwrap();
        broker.mount(path!("ui"), ui).await;

        // Mount InputStore with broker-based dispatcher
        let client_for_dispatch = broker.client();
        let bindings = vec![
            Binding {
                context: BindingContext {
                    mode: "normal".to_string(),
                    key: "j".to_string(),
                    screen: Some("inbox".to_string()),
                },
                action: Action::Command {
                    target: path!("ui/select_next"),
                    fields: vec![],
                },
                description: "Move down".to_string(),
            },
            Binding {
                context: BindingContext {
                    mode: "normal".to_string(),
                    key: "j".to_string(),
                    screen: Some("thread".to_string()),
                },
                action: Action::Command {
                    target: path!("ui/scroll_down"),
                    fields: vec![],
                },
                description: "Scroll down".to_string(),
            },
        ];
        let mut input = InputStore::new(bindings);
        input.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(client_for_dispatch.write(target, data))
            })
        }));
        broker.mount(path!("input"), input).await;

        let client = broker.client();

        // Verify initial state
        let row = client
            .read(&path!("ui/selected_row"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(0));

        // Dispatch "j" on inbox screen → should select_next
        let mut event = BTreeMap::new();
        event.insert("mode".to_string(), Value::String("normal".to_string()));
        event.insert("key".to_string(), Value::String("j".to_string()));
        event.insert("screen".to_string(), Value::String("inbox".to_string()));
        client
            .write(&path!("input/key"), Record::parsed(Value::Map(event)))
            .await
            .unwrap();

        // Verify state changed
        let row = client
            .read(&path!("ui/selected_row"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(1));
    }
}
