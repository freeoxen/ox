//! horns-core: path-MVU UI framework primitives.
//!
//! See `crates/horns/docs/` for the full reader documentation.

pub mod binding;
pub mod command;
pub mod dispatch;
pub mod key;
pub(crate) mod path_serde;
pub mod render;
pub mod view;
pub mod write;

pub use binding::{BindingEntry, BindingId, BindingRegistry, BindingScope, Phase};
pub use command::{
    Command, CommandCtx, CommandDisplay, CommandId, CommandMetadata, CommandRegistry, CommandScope,
};
pub use dispatch::Dispatcher;
pub use key::{KeyChord, KeyCodeRepr, KeyModifierSet};
pub use render::{AscendRule, Rect, RenderCtx, Renderer, RendererMetadata, RendererRegistry};
pub use view::View;
pub use write::Write;
