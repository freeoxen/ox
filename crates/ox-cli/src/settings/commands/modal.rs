//! Modal toggles invoked from the settings screen — small wrappers
//! over the legacy verb-write path so the shared dialog state
//! (`dialog.show_shortcuts`, etc.) stays the single source of truth.
//!
//! These commands write to `ui/<verb>` paths handled by `UiStore`'s
//! pending-action arms (see `crates/ox-ui/src/ui_store.rs`'s
//! `Writer::write` for `toggle_shortcuts` and friends). The action
//! executor flips the dialog flag on the next tick; the shortcuts
//! modal then renders itself from the key-hint stream.

use ox_path::oxpath;
use ox_types::Screen;
use ox_types::subscription::Write;
use structfs_core_store::{Record, Value};

use crate::settings::command_registry::CommandRegistry;

#[allow(unused_imports)]
use super::command;

command! {
    struct_name: ToggleShortcuts,
    id: "modal.toggle_shortcuts",
    title: "Show shortcuts",
    description: "Toggle the shortcuts modal.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "toggle_shortcuts"),
        record: Record::parsed(Value::Null),
    }],
}

pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(ToggleShortcuts::new()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::command_registry::{Command, CommandCtx};
    use crate::settings::registry::RendererRegistry;
    use ox_store_util::local_config::LocalConfig;
    use ox_types::CommandId;

    #[test]
    fn toggle_shortcuts_writes_legacy_verb() {
        let cmd = ToggleShortcuts::new();
        let renderers = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &renderers,
            last_keystroke: None,
        };
        let mut snap = LocalConfig::default();
        let writes = cmd.run(&mut snap, &ctx);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "toggle_shortcuts"));
        assert_eq!(cmd.id(), &CommandId(String::from("modal.toggle_shortcuts")));
    }
}
