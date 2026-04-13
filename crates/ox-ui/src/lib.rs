//! UI stores for ox — state machines driven by typed StructFS command protocol.
//!
//! Stores are synchronous Reader/Writer implementations. Reads return
//! current state as typed snapshots. Writes accept typed UiCommand enums
//! that transition state atomically with screen-scoped guards.

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
pub use builtin_commands::builtin_commands;
// Legacy Command/TxnLog removed — UiStore uses typed UiCommand protocol.
pub use command_def::{
    CommandDef, CommandError, CommandInvocation, ParamDef, ParamKind, StaticCommandDef,
    StaticParamDef, StaticParamKind,
};
pub use command_registry::CommandRegistry;
pub use command_store::CommandStore;
pub use config_store::ConfigStore;
pub use input_store::{Action, Binding, BindingContext, InputStore};
pub use ox_types::ApprovalRequest;
pub use ox_types::{InsertContext, Mode, PendingAction, Screen};
pub use ui_store::UiStore;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use ox_types::{GlobalCommand, InboxCommand, UiCommand};
    use structfs_core_store::{Reader, Record, Value, Writer, path};

    fn typed_cmd(cmd: &UiCommand) -> Record {
        Record::parsed(structfs_serde_store::to_value(cmd).unwrap())
    }

    /// Direct typed-command writes through the broker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typed_command_through_broker_changes_ui_state() {
        let broker = ox_broker::BrokerStore::default();

        // Mount UiStore, set up row count via typed command
        let mut ui = UiStore::new();
        ui.write(
            &path!(""),
            typed_cmd(&UiCommand::Inbox(InboxCommand::SetRowCount { count: 10 })),
        )
        .unwrap();
        broker.mount(path!("ui"), ui).await;

        let client = broker.client();

        // Verify initial state
        let row = client
            .read(&path!("ui/selected_row"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(0));

        // Write typed SelectNext command through broker
        client
            .write(
                &path!("ui"),
                typed_cmd(&UiCommand::Inbox(InboxCommand::SelectNext)),
            )
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

    /// Screen transitions through the broker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn screen_transition_through_broker() {
        let broker = ox_broker::BrokerStore::default();
        broker.mount(path!("ui"), UiStore::new()).await;

        let client = broker.client();

        // Verify starts on inbox
        let screen = client.read(&path!("ui/screen")).await.unwrap().unwrap();
        assert_eq!(screen.as_value().unwrap(), &Value::String("inbox".into()));

        // Open thread
        client
            .write(
                &path!("ui"),
                typed_cmd(&UiCommand::Global(GlobalCommand::Open {
                    thread_id: "t_001".to_string(),
                })),
            )
            .await
            .unwrap();

        let screen = client.read(&path!("ui/screen")).await.unwrap().unwrap();
        assert_eq!(screen.as_value().unwrap(), &Value::String("thread".into()));

        // Close back to inbox
        client
            .write(
                &path!("ui"),
                typed_cmd(&UiCommand::Global(GlobalCommand::Close)),
            )
            .await
            .unwrap();

        let screen = client.read(&path!("ui/screen")).await.unwrap().unwrap();
        assert_eq!(screen.as_value().unwrap(), &Value::String("inbox".into()));
    }
}
