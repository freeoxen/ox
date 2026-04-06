//! UI stores for ox — state machines driven by StructFS command protocol.
//!
//! Stores are synchronous Reader/Writer implementations. Reads return
//! current state. Writes are commands validated by the command protocol
//! (preconditions + txn deduplication).

pub mod command;
pub mod ui_store;

pub use command::{Command, TxnLog};
pub use ui_store::UiStore;
