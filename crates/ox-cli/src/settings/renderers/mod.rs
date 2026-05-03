//! Concrete `Renderer` impls per page (Phases J/K).
//!
//! Each renderer is a pure `&mut dyn Reader -> View` function. Composition
//! is value-shaped: overlay renderers recurse into the `RendererRegistry` to
//! get the background View and wrap it in a `View::Modal`.
//!
//! `register_all` is invoked once at settings-screen startup to install
//! every renderer at its prescribed cursor path.

pub mod index;
pub mod overlay_delete_account;
pub mod overlay_new_account;
pub(crate) mod util;

/// Register every settings renderer at its prescribed cursor path.
pub fn register_all(reg: &mut crate::settings::registry::RendererRegistry) {
    index::register(reg);
    overlay_new_account::register(reg);
    overlay_delete_account::register(reg);
}
