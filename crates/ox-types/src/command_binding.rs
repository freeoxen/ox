//! Compatibility shim while the binding registry is still in ox-cli.
//! Task 4 moves BindingEntry into horns-core/src/binding.rs; this
//! file disappears then.

use serde::{Deserialize, Serialize};

pub use horns_core::binding::{BindingId, BindingScope, Phase};
pub use horns_core::command::{CommandDisplay, CommandId, CommandScope};
pub use horns_core::key::KeyChord;

/// One row in the binding registry: under (scope, phase),
/// the keystroke `key` invokes `command_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    pub scope: BindingScope,
    pub key: KeyChord,
    pub phase: Phase,
    pub command_id: CommandId,
}
