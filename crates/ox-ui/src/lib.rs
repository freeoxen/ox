//! UI stores for ox — state machines driven by StructFS command protocol.
//!
//! Stores are synchronous Reader/Writer implementations. Reads return
//! current state. Writes are commands validated by the command protocol
//! (preconditions + txn deduplication).

pub mod approval_store;
pub mod builtin_commands;
pub mod command;
pub mod command_def;
pub mod command_registry;
pub mod command_store;
pub mod config_store;
pub mod input_store;
pub mod text_input_store;
pub mod ui_store;

pub use approval_store::ApprovalStore;
pub use ox_types::ApprovalRequest;
pub use builtin_commands::builtin_commands;
pub use command::{Command, TxnLog};
pub use command_def::{
    CommandDef, CommandError, CommandInvocation, ParamDef, ParamKind, StaticCommandDef,
    StaticParamDef, StaticParamKind,
};
pub use command_registry::CommandRegistry;
pub use command_store::CommandStore;
pub use config_store::ConfigStore;
pub use input_store::{Action, Binding, BindingContext, InputStore};
pub use ox_types::{InsertContext, Mode, PendingAction, Screen};
pub use text_input_store::TextInputStore;
pub use ui_store::UiStore;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, Writer, path};

    /// End-to-end: key event → InputStore → CommandStore → UiStore state change.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn key_event_through_broker_changes_ui_state() {
        let broker = ox_broker::BrokerStore::default();

        // Mount UiStore with rows so selection can advance
        let mut ui = UiStore::new();
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

        // Mount CommandStore with dispatcher that forwards to broker
        let cmd_client = broker.client();
        let mut cmd_store = CommandStore::from_builtins();
        cmd_store.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(cmd_client.write(target, data))
            })
        }));
        broker.mount(path!("command"), cmd_store).await;

        // Mount InputStore with Action::Invoke binding
        let input_client = broker.client();
        let bindings = vec![
            Binding {
                context: BindingContext {
                    mode: "normal".to_string(),
                    key: "j".to_string(),
                    screen: Some("inbox".to_string()),
                },
                action: Action::Invoke {
                    command: "select_next".to_string(),
                    args: BTreeMap::new(),
                },
                description: "Move down".to_string(),
            },
            Binding {
                context: BindingContext {
                    mode: "normal".to_string(),
                    key: "j".to_string(),
                    screen: Some("thread".to_string()),
                },
                action: Action::Invoke {
                    command: "scroll_down".to_string(),
                    args: BTreeMap::new(),
                },
                description: "Scroll down".to_string(),
            },
        ];
        let mut input = InputStore::new(bindings);
        input.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(input_client.write(target, data))
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

        // Dispatch "j" on inbox screen → InputStore → CommandStore → UiStore
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

    /// End-to-end: Action::Invoke → CommandStore → UiStore state change.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invoke_action_through_command_store() {
        let broker = ox_broker::BrokerStore::default();

        // Mount UiStore with rows so selection can advance
        let mut ui = UiStore::new();
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

        // Mount CommandStore with dispatcher that forwards to broker
        let cmd_client = broker.client();
        let mut cmd_store = CommandStore::from_builtins();
        cmd_store.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(cmd_client.write(target, data))
            })
        }));
        broker.mount(path!("command"), cmd_store).await;

        // Mount InputStore with Action::Invoke binding
        let input_client = broker.client();
        let bindings = vec![Binding {
            context: BindingContext {
                mode: "normal".to_string(),
                key: "j".to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command: "select_next".to_string(),
                args: BTreeMap::new(),
            },
            description: "Move down".to_string(),
        }];
        let mut input = InputStore::new(bindings);
        input.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(input_client.write(target, data))
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

        // Dispatch "j" → InputStore → CommandStore → UiStore
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
