//! Settings-screen renderers, commands, bindings, and registry.
//!
//! Tree shape:
//! - `renderers`            — concrete Renderer impls per page.
//! - `commands`             — Command impls.
//! - `bindings`             — BindingRegistry registration.
//! - `bootstrap`            — boot-time registration entry point.
//! - `dispatch`             — settings-screen dispatcher wiring.
//! - `help`                 — key-hint projection.
//! - `snapshot`             — pre-render snapshot builder.
//! - `visible_rows`         — visible-row projection.
//!
//! The framework registries themselves (`BindingRegistry`,
//! `CommandRegistry`, `RendererRegistry`) live in `horns_core` and are
//! re-exported below for backwards-compatible call sites.

pub mod bindings;
pub mod bootstrap;
pub mod commands;
pub mod help;
pub mod renderers;
pub mod snapshot;
pub mod visible_rows;

pub use horns_core::{
    AscendRule, BindingRegistry, Command, CommandCtx, CommandRegistry, RenderCtx, Renderer,
    RendererRegistry,
};
