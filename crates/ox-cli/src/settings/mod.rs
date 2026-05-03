//! Settings-screen renderers, commands, bindings, and registry.
//!
//! Tree shape:
//! - `registry`             — Renderer trait + RendererRegistry + AscendRule (Phase G).
//! - `renderers`            — concrete Renderer impls per page (Phases J/K).
//! - `commands`             — Command impls (Phase L).
//! - `bindings`             — BindingRegistry registration (Phase M).
//! - `subscription_install` — wire ox-gate subscriptions at startup (Phase N).
//! - `snapshot`             — pre-render snapshot builder (Phase I).
//!
//! Future modules below are commented out until their phase lands; uncomment
//! the line when wiring up that phase's first task. The doc list above is
//! the source of truth for the eventual layout — keep it in sync with the
//! actual `mod` declarations as work progresses.

pub mod binding_registry;
pub mod bindings;
pub mod bootstrap;
pub mod command_registry;
pub mod commands;
pub mod dispatch;
pub mod help;
pub mod registry;
pub mod renderers;
pub mod snapshot;
pub mod visible_rows;
// pub(crate) mod subscription_install; // Phase N
