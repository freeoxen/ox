//! horns install API — produces the data needed to mount one horns
//! instance as a broker subscription set.
//!
//! `build_install_bundle` is pure: it returns an `InstallBundle` of
//! `(metadata_writes, subscriptions)` that the host then applies to
//! whatever broker it owns. This keeps horns-core broker-agnostic — the
//! install API never touches `ox-broker` itself.
//!
//! ## Side tables
//!
//! The returned subscriptions share an `Arc<RwLock<SideTables>>` that
//! holds:
//!
//! - the `BindingRegistry` built from `opts.bindings` and
//!   `opts.handler_metadata`'s `Arc<dyn KeyHandler>` entries
//! - the `CommandRegistry` built from `opts.commands`
//! - the `RendererRegistry` built from `opts.renderers`
//!
//! Each subscription's `handle` acquires a read lock on the side tables
//! and dispatches against them. The host can rebuild the side tables and
//! re-install for live updates; the data-on-broker copies (under
//! `bindings_prefix` etc.) are present so introspection tools (help
//! hints, palette) can see what's installed without reaching into
//! horns-core.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;
use structfs_core_store::{Path, Record, Value};

use crate::binding::{
    BindingEntry, BindingId, BindingRegistry, HandlerEntry, HandlerId, HandlerMetadata, KeyHandler,
};
use crate::command::{Command, CommandId, CommandMetadata, CommandRegistry};
use crate::dispatch::Dispatcher;
use crate::render::{Renderer, RendererRegistry};
use crate::subscription::{PathPattern, SubCtx, Subscription, SubscriptionId};
use crate::write::Write;

/// Inputs to `build_install_bundle`. All path fields are absolute (they
/// live in the host's broker namespace); the host chooses where each
/// goes. `commands`, `renderers`, `handlers` are by-id maps the host
/// uses to look up which closure to invoke at dispatch time.
pub struct InstallOptions {
    /// Path the focus cursor is read from on every dispatch.
    pub cursor_path: Path,
    /// Subtree the host writes `KeyChord` records under
    /// (e.g. `<input_path>/key`).
    pub input_path: Path,
    /// Path bumped by the dispatcher (and by theme changes) to trigger
    /// the render subscription.
    pub render_tick_path: Path,
    /// Path the render subscription writes its View to.
    pub render_output_path: Path,
    /// Prefix under which `BindingEntry` records are persisted, one per
    /// binding (`<bindings_prefix>/<binding-id>`).
    pub bindings_prefix: Path,
    /// Prefix under which `CommandMetadata` records are persisted.
    pub commands_prefix: Path,
    /// Prefix under which `RendererMetadata` records are persisted.
    pub renderers_prefix: Path,
    /// Prefix under which `HandlerMetadata` records are persisted.
    pub handlers_prefix: Path,
    /// Path the theme JSON is written to. Writes here bump the render
    /// tick.
    pub theme_path: Path,

    /// Built-in commands by id. Owned and held inside the side tables.
    pub commands: HashMap<CommandId, Box<dyn Command>>,
    /// Built-in renderers keyed by cursor `Path`. Owned and held inside
    /// the side tables.
    pub renderers: HashMap<Path, Box<dyn Renderer>>,
    /// Opaque key handlers by id. Held as `Arc<dyn KeyHandler>` so the
    /// handler tier of the binding registry can reference them.
    pub handlers: HashMap<HandlerId, Arc<dyn KeyHandler>>,

    /// Bindings to register. Each `BindingId` is the path component
    /// under `<bindings_prefix>` so introspection tools can find the
    /// row that maps to a key.
    pub bindings: Vec<(BindingId, BindingEntry)>,
    /// Handler metadata rows to register. The id pairs with the key in
    /// `handlers` — the data half is persisted under
    /// `<handlers_prefix>/<handler-id>` and the trait half is invoked
    /// at dispatch time.
    pub handler_metadata: Vec<(HandlerId, HandlerMetadata)>,
    /// Theme to write at `theme_path`. Opaque JSON — horns-core has no
    /// concrete theme type.
    pub theme: serde_json::Value,
}

/// Output of `build_install_bundle`. The host writes the metadata first,
/// then registers the subscriptions on its broker.
pub struct InstallBundle {
    /// The three subscriptions that run the horns instance:
    /// KeyDispatch (input → command writes), Render
    /// (render_tick / cursor → View), and ThemeChange (theme → render_tick).
    pub subscriptions: Vec<Arc<dyn Subscription>>,
    /// Path metadata to apply *before* registering the subscriptions.
    /// Contains binding rows, command/renderer/handler metadata rows,
    /// and the initial theme.
    pub metadata_writes: Vec<(Path, Record)>,
}

/// Handle returned to the host so it can later unregister or supersede
/// a horns install. Today it's just the three subscription ids; future
/// shapes (per-id dispatch counters, etc.) may add fields.
pub struct HornsHandle {
    pub subscription_ids: Vec<SubscriptionId>,
}

/// Side tables shared across all three subscriptions. Held behind
/// `Arc<RwLock<...>>` so subscriptions can read at dispatch time
/// without holding a long-lived reference; the host can mutate
/// (re-install) via `write()` if needed.
pub(crate) struct SideTables {
    pub bindings: BindingRegistry,
    pub commands: CommandRegistry,
    pub renderers: RendererRegistry,
}

/// Paths the install pipeline threads through into the three
/// subscriptions. Separated from `InstallOptions` so the
/// "pre-built registries" path can share it without dragging in
/// the by-id maps `InstallOptions` collects for introspection.
#[derive(Clone)]
pub struct InstallPaths {
    pub cursor_path: Path,
    pub input_path: Path,
    pub render_tick_path: Path,
    pub render_output_path: Path,
    pub theme_path: Path,
}

/// Build the install bundle from options. Pure (no broker interaction);
/// the host applies the returned writes and registers the returned
/// subscriptions.
pub fn build_install_bundle(opts: InstallOptions) -> InstallBundle {
    // ---- 1. Build side tables (bindings + commands + renderers). ----
    let mut binding_registry = BindingRegistry::new();
    let mut command_registry = CommandRegistry::new();
    let mut renderer_registry = RendererRegistry::new();

    // Discrete bindings.
    for (_id, entry) in &opts.bindings {
        binding_registry.register(entry.clone());
    }

    // Handler tier: each HandlerMetadata pairs with an Arc<dyn KeyHandler>
    // in opts.handlers. Skip metadata rows whose handler isn't registered
    // (defensive — keeps misconfigured installs from panicking; the
    // missing entry just won't fire).
    for (id, meta) in &opts.handler_metadata {
        if let Some(handler) = opts.handlers.get(id) {
            binding_registry.register_handler(HandlerEntry {
                scope: meta.scope.clone(),
                phase: meta.phase,
                handler: handler.clone(),
            });
        }
    }

    // Commands and renderers: drain the maps into the registries.
    let mut commands_drain = opts.commands;
    for (_id, cmd) in commands_drain.drain() {
        command_registry.register(cmd);
    }
    let mut renderers_drain = opts.renderers;
    for (cursor, renderer) in renderers_drain.drain() {
        renderer_registry.register(cursor, renderer);
    }

    let side_tables = Arc::new(RwLock::new(SideTables {
        bindings: binding_registry,
        commands: command_registry,
        renderers: renderer_registry,
    }));

    // ---- 2. Collect metadata writes. ----
    let mut metadata_writes: Vec<(Path, Record)> = Vec::new();

    // Bindings: <bindings_prefix>/<binding-id>
    for (id, entry) in &opts.bindings {
        let path = path_join(&opts.bindings_prefix, &id.0);
        metadata_writes.push((path, record_from_serde(entry)));
    }

    // Command metadata: <commands_prefix>/<command-id>
    // The CommandRegistry now owns the boxes; iter gives us &dyn Command,
    // from which we extract id/display/scope to materialize
    // CommandMetadata. We do this *after* draining into the registry
    // so we don't have to keep two copies.
    {
        let tables = side_tables.read().expect("side tables poisoned");
        for cmd in tables.commands.iter() {
            let meta = CommandMetadata {
                display: cmd.display().clone(),
                scope: cmd.scope().clone(),
            };
            let path = path_join(&opts.commands_prefix, &cmd.id().0);
            metadata_writes.push((path, record_from_serde(&meta)));
        }
    }

    // Renderer metadata isn't emitted at install time. RendererRegistry
    // doesn't expose an iterator and we've already drained the input
    // map. Hosts that want renderer metadata on the broker for
    // introspection can write it themselves; this is a deliberate gap
    // for now.

    // Handler metadata: <handlers_prefix>/<handler-id>
    for (id, meta) in &opts.handler_metadata {
        let path = path_join(&opts.handlers_prefix, &id.0);
        metadata_writes.push((path, record_from_serde(meta)));
    }

    // Theme: opts.theme_path
    metadata_writes.push((
        opts.theme_path.clone(),
        Record::parsed(json_to_value(opts.theme)),
    ));

    // ---- 3. Build the three subscriptions. ----
    let subscriptions = build_subscriptions(
        side_tables,
        InstallPaths {
            cursor_path: opts.cursor_path,
            input_path: opts.input_path,
            render_tick_path: opts.render_tick_path,
            render_output_path: opts.render_output_path,
            theme_path: opts.theme_path,
        },
    );

    InstallBundle {
        subscriptions,
        metadata_writes,
    }
}

/// Build the install bundle from pre-populated registries. The legacy
/// `build_install_bundle` rebuilds registries from the by-id maps in
/// `InstallOptions` to also emit `<bindings_prefix>/<id>` introspection
/// writes; hosts whose registration helpers don't carry per-entry ids
/// can construct the registries directly and skip the binding /
/// command / renderer metadata writes. The theme record is still
/// written so `ThemeChangeSubscription` has something to read on its
/// first fire.
pub fn build_install_bundle_from_registries(
    bindings: BindingRegistry,
    commands: CommandRegistry,
    renderers: RendererRegistry,
    paths: InstallPaths,
    theme: serde_json::Value,
) -> InstallBundle {
    let side_tables = Arc::new(RwLock::new(SideTables {
        bindings,
        commands,
        renderers,
    }));

    let metadata_writes = vec![(
        paths.theme_path.clone(),
        Record::parsed(json_to_value(theme)),
    )];

    let subscriptions = build_subscriptions(side_tables, paths);

    InstallBundle {
        subscriptions,
        metadata_writes,
    }
}

/// Construct the three runtime subscriptions (KeyDispatch + Render +
/// ThemeChange) over a populated `SideTables`. Shared by both
/// `build_install_bundle` and `build_install_bundle_from_registries`.
fn build_subscriptions(
    side_tables: Arc<RwLock<SideTables>>,
    paths: InstallPaths,
) -> Vec<Arc<dyn Subscription>> {
    let key_path = path_join(&paths.input_path, "key");
    let area_path = path_join(&paths.input_path, "area");

    let key_dispatch = Arc::new(KeyDispatchSubscription {
        id: SubscriptionId("horns.key_dispatch".to_string()),
        watches: vec![PathPattern::Exact(key_path.clone())],
        side_tables: side_tables.clone(),
        dispatcher: Dispatcher::new(paths.cursor_path.clone()),
        render_tick_path: paths.render_tick_path.clone(),
    });

    let render_sub = Arc::new(RenderSubscription {
        id: SubscriptionId("horns.render".to_string()),
        watches: vec![
            PathPattern::Exact(paths.render_tick_path.clone()),
            PathPattern::Exact(paths.cursor_path.clone()),
            PathPattern::Exact(area_path.clone()),
        ],
        side_tables: side_tables.clone(),
        cursor_path: paths.cursor_path.clone(),
        area_path,
        render_output_path: paths.render_output_path.clone(),
    });

    let theme_sub = Arc::new(ThemeChangeSubscription {
        id: SubscriptionId("horns.theme_change".to_string()),
        watches: vec![PathPattern::Exact(paths.theme_path.clone())],
        render_tick_path: paths.render_tick_path.clone(),
    });

    vec![
        key_dispatch as Arc<dyn Subscription>,
        render_sub as Arc<dyn Subscription>,
        theme_sub as Arc<dyn Subscription>,
    ]
}

// ---------------------------------------------------------------------------
// Subscription implementations
// ---------------------------------------------------------------------------

/// Watches `<input_path>/key`. On a `KeyChord` write, reads the focus
/// cursor and runs the horns `Dispatcher` against the side tables. The
/// returned writes are appended with a render-tick bump so the render
/// subscription wakes up.
struct KeyDispatchSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    side_tables: Arc<RwLock<SideTables>>,
    dispatcher: Dispatcher,
    render_tick_path: Path,
}

impl Subscription for KeyDispatchSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        // Decode the KeyChord from the change's `after` record.
        let Some(after) = &ctx.change.after else {
            return Vec::new();
        };
        let Some(value) = after.as_value() else {
            return Vec::new();
        };
        let Ok(key) =
            structfs_serde_store::from_value::<crate::key::KeyChord>(value.clone())
        else {
            return Vec::new();
        };

        // Dispatch through the side tables.
        let tables = self.side_tables.read().expect("side tables poisoned");
        let mut writes = self.dispatcher.dispatch(
            ctx.snapshot,
            &key,
            &tables.bindings,
            &tables.commands,
            &tables.renderers,
        );

        // Bump the render tick so the render subscription re-renders.
        // The tick is a monotonically increasing counter; we read the
        // current value and write +1. Treat any non-Integer / missing
        // value as 0.
        let current = ctx
            .snapshot
            .read(&self.render_tick_path)
            .ok()
            .flatten()
            .and_then(|r| match r.as_value() {
                Some(Value::Integer(n)) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        writes.push(Write {
            path: self.render_tick_path.clone(),
            record: Record::parsed(Value::Integer(current.wrapping_add(1))),
        });

        writes
    }
}

/// Watches `<render_tick_path>` and `<cursor_path>`. On either, reads
/// the cursor, runs the renderer at that cursor, and writes the
/// resulting View (as a serde_json::Value via Record) to
/// `<render_output_path>`.
struct RenderSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    side_tables: Arc<RwLock<SideTables>>,
    cursor_path: Path,
    area_path: Path,
    render_output_path: Path,
}

impl Subscription for RenderSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        // Read the focus cursor; if not set, no-op.
        let cursor = match read_cursor(ctx.snapshot, &self.cursor_path) {
            Some(p) => p,
            None => return Vec::new(),
        };

        // Read the current area. Default to a sensible 80x24 if the host
        // hasn't written one yet — the first render fires at install time
        // before the event loop has had a chance to seed the terminal
        // size, and the first frame after that gets the real size.
        let area = ctx
            .snapshot
            .read(&self.area_path)
            .ok()
            .flatten()
            .and_then(|r| r.as_value().cloned())
            .and_then(|v| structfs_serde_store::from_value::<crate::render::Rect>(v).ok())
            .unwrap_or_else(|| crate::render::Rect::new(0, 0, 80, 24));

        let tables = self.side_tables.read().expect("side tables poisoned");
        let theme: () = ();
        let mut ctx_render = crate::render::RenderCtx {
            area,
            data: ctx.snapshot,
            registry: &tables.renderers,
            theme: &theme as &dyn std::any::Any,
        };
        let view = tables.renderers.render(&cursor, &mut ctx_render);

        let view_value = match structfs_serde_store::to_value(&view) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        vec![Write {
            path: self.render_output_path.clone(),
            record: Record::parsed(view_value),
        }]
    }
}

/// Watches the theme path. On change, bumps `<render_tick_path>` so the
/// render subscription re-runs with the new theme.
struct ThemeChangeSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    render_tick_path: Path,
}

impl Subscription for ThemeChangeSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        let current = ctx
            .snapshot
            .read(&self.render_tick_path)
            .ok()
            .flatten()
            .and_then(|r| match r.as_value() {
                Some(Value::Integer(n)) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        vec![Write {
            path: self.render_tick_path.clone(),
            record: Record::parsed(Value::Integer(current.wrapping_add(1))),
        }]
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Join `prefix` with a single path component (e.g. an id segment).
/// Falls back to `prefix` unchanged if the segment is empty — the
/// caller-provided BindingId / CommandId / HandlerId should never be
/// empty, but we avoid panicking on the off chance.
fn path_join(prefix: &Path, segment: &str) -> Path {
    if segment.is_empty() {
        return prefix.clone();
    }
    let mut components = prefix.components.clone();
    components.push(segment.to_string());
    Path::try_from_components(components).unwrap_or_else(|_| prefix.clone())
}

/// Serialize a typed value via `structfs_serde_store::to_value` and wrap
/// in a `Record`. Falls back to `Value::Map(empty)` on serialize failure
/// — `BindingEntry`/`CommandMetadata`/`HandlerMetadata` are all derive-
/// generated serde impls, so failure here would be a horns-core bug.
fn record_from_serde<T: Serialize>(value: &T) -> Record {
    match structfs_serde_store::to_value(value) {
        Ok(v) => Record::parsed(v),
        Err(_) => Record::parsed(Value::Map(std::collections::BTreeMap::new())),
    }
}

/// Convert a `serde_json::Value` to a structfs `Value`. JSON's shape is
/// a strict subset of `Value` (no Bytes), so this round-trips through
/// the serde_json string representation via `structfs_serde_store`'s
/// `to_value`. Falls back to an empty map on failure.
fn json_to_value(json: serde_json::Value) -> Value {
    match structfs_serde_store::to_value(&json) {
        Ok(v) => v,
        Err(_) => Value::Map(std::collections::BTreeMap::new()),
    }
}

/// Read the focus cursor encoded as `Value::Array` of `Value::String`
/// segments (the wire shape used by horns navigation commands). Returns
/// `None` if the path is unset or has a different shape.
fn read_cursor(snapshot: &mut dyn structfs_core_store::Reader, path: &Path) -> Option<Path> {
    let record = snapshot.read(path).ok().flatten()?;
    let value = record.as_value()?;
    match value {
        Value::Array(items) => {
            let mut components: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => components.push(s.clone()),
                    _ => return None,
                }
            }
            Path::try_from_components(components).ok()
        }
        _ => None,
    }
}
