//! horns-core: path-MVU UI framework primitives.
//!
//! See `crates/horns/docs/` for the full reader documentation.

pub mod binding;
pub mod command;
pub mod dispatch;
pub mod install;
pub mod key;
pub(crate) mod path_serde;
pub mod render;
pub mod subscription;
pub mod view;
pub mod write;

pub use binding::{
    BindingEntry, BindingId, BindingRegistry, BindingScope, HandlerEntry, HandlerId,
    HandlerMetadata, KeyHandler, Phase,
};
pub use command::{
    Command, CommandCtx, CommandDisplay, CommandId, CommandMetadata, CommandRegistry, CommandScope,
};
pub use dispatch::Dispatcher;
pub use install::{build_install_bundle, HornsHandle, InstallBundle, InstallOptions};
pub use key::{KeyChord, KeyCodeRepr, KeyModifierSet};
pub use render::{AscendRule, Rect, RenderCtx, Renderer, RendererMetadata, RendererRegistry};
pub use subscription::{
    AsyncWriter, BoxFuture, PathChange, PathPattern, SpawnHandle, SubCtx, Subscription,
    SubscriptionId, SubscriptionRegistry,
};
pub use view::View;
pub use write::Write;
