//! Settings-screen renderers, commands, bindings, and registry.
//!
//! Tree shape:
//! - `renderers`            — concrete Renderer impls per page.
//! - `commands`             — Command impls.
//! - `bindings`             — BindingRegistry registration.
//! - `bootstrap`            — boot-time registration entry point.
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
pub mod shortcuts;
pub mod snapshot;
pub mod visible_rows;

pub use horns_core::{
    AscendRule, BindingRegistry, Command, CommandCtx, CommandRegistry, RenderCtx, Renderer,
    RendererRegistry,
};

use horns_core::SubscriptionId;
use horns_core::install::{InstallPaths, build_install_bundle_from_registries};
use ox_broker::BrokerStore;
use ox_path::oxpath;
use structfs_core_store::{Error as StoreError, Path};

/// Path the settings install reads the focus cursor from.
///
/// The cursor is *also* the data path the existing settings commands
/// have always written to (`accordion.focus` etc.), so the broker
/// write that fires the focus-change re-render is the same write
/// existing code emits — no new authoring convention needed.
pub fn cursor_path() -> Path {
    oxpath!("ui", "settings", "focused")
}

/// Broker path the event loop writes a `KeyChord` to to drive
/// settings-screen dispatch. The horns `KeyDispatchSubscription`
/// installed by [`install`] watches `<input_path>/key` and reacts.
pub fn input_path() -> Path {
    oxpath!("ui", "_horns", "settings", "input")
}

/// The exact broker path the event loop writes a `KeyChord` to.
/// Centralizes the `key` suffix so the producer (event loop) and the
/// consumer (KeyDispatchSubscription) cannot silently desync.
pub fn input_key_path() -> Path {
    oxpath!("ui", "_horns", "settings", "input", "key")
}

/// The exact broker path the event loop writes the terminal `Rect` to.
/// Writes here trigger `RenderSubscription` to re-render with the new
/// size, so the host writes this on startup and on every terminal
/// resize.
pub fn input_area_path() -> Path {
    oxpath!("ui", "_horns", "settings", "input", "area")
}

/// Broker path the horns render pipeline writes its serialized
/// `View` to. The event loop reads this on every frame and composes
/// it into the overall TUI frame in `tui::draw`.
pub fn render_output_path() -> Path {
    oxpath!("ui", "_horns", "settings", "render", "output")
}

/// Broker path the dispatcher bumps to wake the render subscription.
/// Also bumped on initial install so the subscription fires at least
/// once and produces a View for the first frame.
pub fn render_tick_path() -> Path {
    oxpath!("ui", "_horns", "settings", "render", "tick")
}

/// Path the install writes the (JSON-encoded) theme to. Writes here
/// trigger `ThemeChangeSubscription`, which bumps render-tick.
///
/// Under `ui/_horns/` (the generic substore mounted at install time)
/// rather than `ui/theme` directly — the outer `ui/` mount is a
/// `UiStore` that only accepts known UI-command paths, and `theme`
/// isn't one. Putting theme inside the horns subtree keeps the install
/// writes routable without inventing a new top-level mount.
pub fn theme_path() -> Path {
    oxpath!("ui", "_horns", "theme")
}

/// Broker prefix the install writes `BindingEntry` rows to. The event
/// loop's hint projection reads from this subtree per frame.
pub fn bindings_prefix() -> Path {
    oxpath!("horns", "settings", "bindings")
}

/// Broker prefix the install writes `CommandMetadata` rows to. The
/// event loop's hint projection reads from this subtree per frame.
pub fn commands_prefix() -> Path {
    oxpath!("horns", "settings", "commands")
}

/// Handle returned from [`install`]. Holds the subscription ids the
/// broker registered — useful for future tear-down / re-install
/// semantics. Keeping the handle alive isn't strictly necessary on
/// `BrokerStore` (subscriptions live in an `Arc<RwLock<...>>` inside
/// the broker), but binding it to a variable in main makes the
/// "settings is mounted" lifecycle explicit at the call site.
pub struct SettingsHandle {
    pub subscription_ids: Vec<SubscriptionId>,
}

/// Install the settings screen as a horns instance on `broker`.
///
/// Builds the three framework registries (commands, renderers,
/// bindings + handlers) from the existing `register_*` entry points,
/// wraps them in the install side-tables, and registers
/// `KeyDispatchSubscription`, `RenderSubscription`, and
/// `ThemeChangeSubscription` on the broker. The metadata writes
/// (today: just the initial theme record) are applied before the
/// subscriptions register.
///
/// After install, the event loop's settings-screen path becomes:
///   1. Encode the crossterm key as a `KeyChord`.
///   2. Write it to `input_path()/key`.
///   3. `KeyDispatchSubscription` runs the matched command, emits its
///      writes through the broker dispatcher, bumps `render_tick_path()`.
///   4. `RenderSubscription` wakes on render-tick (or cursor) change,
///      runs the matched renderer at the focused cursor, writes the
///      serialized `View` to `render_output_path()`.
///   5. The event loop reads the View from there and composes it into
///      the overall ratatui frame in `tui::draw`.
///
/// `tui::draw` still owns the *physical* terminal — the subscription
/// produces the View into the broker, the event loop pulls it out, no
/// terminal-mutex contention. Theme changes funnel through the same
/// tick-bump path so a future settings-driven theme switch
/// automatically re-renders without bespoke wiring.
pub async fn install(broker: &BrokerStore) -> Result<SettingsHandle, StoreError> {
    // ---- 1. Build the three registries. ----
    let mut bindings = BindingRegistry::new();
    let mut commands = CommandRegistry::new();
    let mut renderers = RendererRegistry::new();
    crate::settings::bindings::register(&mut bindings);
    crate::settings::commands::register_all(&mut commands);
    crate::settings::renderers::register_all(&mut renderers);

    // ---- 2. Theme: a placeholder record. ----
    //
    // `horns_ratatui::Theme` does not (yet) implement `Serialize`, and
    // the framework's `RenderSubscription` passes `&()` to renderers
    // anyway (Task 8 architectural deviation: theme is structurally
    // wired through `RenderCtx::theme: &dyn Any` but the host can't
    // get a typed handle out of the JSON value). The settings screen
    // therefore still renders with `horns_ratatui::Theme::default()`
    // applied by `tui::draw` against the View we read from the broker
    // — the broker theme record exists only so
    // `ThemeChangeSubscription` has a path to watch, and so a future
    // typed-theme install option can drop in without re-wiring the
    // call site. Until then: any non-null JSON record will do.
    let theme_json = serde_json::json!({});

    // ---- 3. Snapshot bindings + commands for the shortcut resolver
    //         BEFORE moving the registries into the install bundle.
    //         The resolver holds these in-memory and never re-reads
    //         them from the broker — bindings are immutable
    //         infrastructure, and recursive non-leaf reads through
    //         `LocalConfig` are the per-cursor cost we're avoiding.
    let resolver = crate::settings::shortcuts::ShortcutResolver::from_registries(
        cursor_path(),
        crate::settings::shortcuts::shortcuts_path(),
        &bindings,
        &commands,
    )
    .boxed();

    // ---- 4. Build the install bundle (moves bindings + commands). ----
    let paths = InstallPaths {
        cursor_path: cursor_path(),
        input_path: input_path(),
        render_tick_path: render_tick_path(),
        render_output_path: render_output_path(),
        theme_path: theme_path(),
        bindings_prefix: bindings_prefix(),
        commands_prefix: commands_prefix(),
    };
    let bundle =
        build_install_bundle_from_registries(bindings, commands, renderers, paths, theme_json);

    // ---- 5. Apply metadata writes through the broker client. ----
    let client = broker.client();
    for (path, record) in &bundle.metadata_writes {
        client.write(path, record.clone()).await?;
    }

    // ---- 6. Register subscriptions and collect their ids. ----
    //
    // Order matters: the `ShortcutResolver` watches `cursor_path` and
    // produces the active shortcut record at `shortcuts_path()`.
    // `RenderSubscription` *also* watches `cursor_path` and reads
    // `shortcuts_path()` through the IndexRenderer. Registering the
    // resolver first means a cursor write fires it before the render
    // sub, so the render reads the fresh record on the same cascade
    // — no one-frame stale display, no second render needed to catch
    // up. `BrokerStore` honors registration order on a single write
    // (see `ox_broker` spec §3.3).
    let resolver_id = resolver.id().clone();
    broker.register_subscription(resolver);

    let mut subscription_ids: Vec<SubscriptionId> = vec![resolver_id];
    subscription_ids.extend(bundle.subscriptions.iter().map(|s| s.id().clone()));
    for sub in bundle.subscriptions {
        broker.register_subscription(sub);
    }

    // ---- 6. Seed the render tick so the RenderSubscription fires
    //         once with the current cursor and emits an initial View.
    //         Without this, the very first frame the user opens
    //         settings would have to wait on the first keystroke
    //         before any View existed on the broker. We write `0` —
    //         the dispatcher's tick-bump path uses `wrapping_add(1)`
    //         and treats a missing-or-non-Integer record as `0`, so
    //         this also seeds the type the dispatcher expects.
    use structfs_core_store::{Record, Value};
    client
        .write(&render_tick_path(), Record::parsed(Value::Integer(0)))
        .await?;

    // Note: the ratatui ViewRenderSubscription that actually paints
    // the terminal is installed by `run_horns_settings_loop` in the
    // event loop — that's the state that owns the terminal during
    // a horns session. Keeping the install here would force a shared
    // `Arc<Mutex<Terminal>>` at the top level, which fights the
    // state-machine ownership model.

    Ok(SettingsHandle { subscription_ids })
}
