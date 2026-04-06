//! UI stores for ox — state machines driven by StructFS command protocol.
//!
//! Stores are synchronous Reader/Writer implementations. Reads return
//! current state. Writes are commands validated by the command protocol
//! (preconditions + txn deduplication).

pub mod approval_store;
pub mod command;
pub mod input_store;
pub mod ui_store;

pub use approval_store::ApprovalStore;
pub use command::{Command, TxnLog};
pub use input_store::{Binding, InputStore};
pub use ui_store::UiStore;
