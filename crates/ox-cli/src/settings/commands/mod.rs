//! Settings commands — pure functions from snapshot to writes.
//!
//! Each command is a small `Command` impl. Day-one commands fall into three
//! buckets:
//!
//! - `highlight`     — per-area selection cycling (index/accounts/models).
//! - `navigation`    — descend/ascend cursor changes (Phase L2).
//! - `account_model` — account/model/field operations + selectors (Phase L3).
//!
//! `register_all` (Phase L4) is invoked once at settings-screen startup to
//! install every day-one command into the `CommandRegistry`.

/// Common shape for a `Command` impl: stable id, display, scope, and a
/// `run` body. The macro builds a struct + Command impl from a closure-shaped
/// body. Cuts ~12 lines of boilerplate per command.
///
/// The `run` body has access to two named parameters (`$snap` and `$ctx`)
/// that bind to `&mut dyn Reader` and `&CommandCtx<'_>` respectively. The
/// body is a single expression returning `Vec<Write>`.
macro_rules! command {
    (
        struct_name: $name:ident,
        id: $id:literal,
        title: $title:literal,
        description: $desc:literal,
        screen: $screen:expr,
        cursor: $cursor:expr,
        run: |$snap:ident, $ctx:ident| $body:expr $(,)?
    ) => {
        pub struct $name {
            id: ox_types::CommandId,
            display: ox_types::CommandDisplay,
            scope: ox_types::CommandScope,
        }
        impl $name {
            pub fn new() -> Self {
                Self {
                    id: ox_types::CommandId(String::from($id)),
                    display: ox_types::CommandDisplay {
                        name: String::from($title),
                        description: String::from($desc),
                    },
                    scope: ox_types::CommandScope {
                        screen: $screen,
                        cursor_path: $cursor,
                    },
                }
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl crate::settings::command_registry::Command for $name {
            fn id(&self) -> &ox_types::CommandId {
                &self.id
            }
            fn display(&self) -> &ox_types::CommandDisplay {
                &self.display
            }
            fn scope(&self) -> &ox_types::CommandScope {
                &self.scope
            }
            fn run(
                &self,
                $snap: &mut dyn structfs_core_store::Reader,
                $ctx: &crate::settings::command_registry::CommandCtx<'_>,
            ) -> Vec<ox_types::subscription::Write> {
                let _ = $ctx; // some commands ignore ctx
                $body
            }
        }
    };
}

pub(crate) use command;

pub mod highlight;
pub mod navigation;
