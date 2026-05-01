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

pub(crate) mod binding_registry;
pub(crate) mod command_registry;
pub(crate) mod dispatch;
pub(crate) mod registry;
pub(crate) mod snapshot;
// pub(crate) mod renderers;            // Phase J/K
// pub(crate) mod commands;             // Phase L
// pub(crate) mod bindings;             // Phase M
// pub(crate) mod subscription_install; // Phase N
