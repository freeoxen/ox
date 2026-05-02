# Settings screen redesign — path-based MVU

**Date:** 2026-04-27 (revised 2026-04-28)
**Status:** Design — pending implementation plan
**Crates touched:** `ox-cli`, `ox-gate`, `ox-kernel`, `ox-types`, `ox-broker`, new `ox-view`

## 1. Summary

The current settings screen is a flat two-pane editor (Accounts + Defaults) with `Tab` toggling between sections, an overlay modal for account editing, and a parallel state machine for the first-run wizard. Three problems compound:

1. The screen is consumed by what is structurally one slice of settings — provider/account/model configuration — leaving no shape for future categories (appearance, keybindings, telemetry).
2. Tab-as-section-switch is a second navigation axis on top of j/k/arrow movement, which confuses keyboard users.
3. The "Defaults" pane is structurally broken: a global `default_model` is incoherent across providers, the model picker silently depends on which account was most recently tested in the modal, and `max_tokens` is a free-text integer that doesn't match how anyone thinks about token budgets.

Settings is also the only client-side state machine in an otherwise broker-authoritative app (`event_loop.rs:773` explicitly bypasses the binding table for it). That misalignment is the root cause of the awkward navigation; bringing settings into the broker pattern fixes it by construction.

This document proposes:

- **A reusable architectural primitive — path-based MVU** — for screens that are projections of typed namespace state. Three trees (data, display, view), one update loop, subscriptions as a protocol over StructFS. Settings is the first user.
- **A concrete redesign of the settings screen** as a projection over a UI-shaped *display tree*, with a renderer registry that emits a typed `View` tree, commands as Rust trait impls, and per-area selection pointers held in the namespace.
- **Data-shape changes** to make the catalog the source of truth, replace global "defaults" with a typed completion-role tag, and decouple the first-run wizard.

## 2. Goals & non-goals

### Goals

- Replace the flat two-pane settings screen with a navigable settings hub. Adding a new category is a registration call, not a state-machine change.
- Eliminate the bespoke `crate::settings_shell::handle_key` bypass at `event_loop.rs:773`. Settings dispatches through the same binding-table mechanism every other screen uses.
- Make the catalog the source of truth for `(model_id, max_context_size, max_output_tokens)`. Drop the user-facing free-text model id and `max_tokens` fields.
- Provide a forward-compatible shape for completion-role tagging (`primary` today, `summarizer` and others tomorrow).
- Provide a forward-compatible shape for per-thread completion overrides via the existing `Cascade<thread_overlay, base>` overlay primitive.
- Establish path-based MVU as a documented, reusable primitive for future screens. Renderers as pure `&Reader -> View` functions; commands as pure `&Reader -> Vec<Write>` functions; subscriptions as a small broker-side protocol over StructFS.

### Non-goals

- The first-run wizard. Decoupled and implemented as a separate modal flow; out of scope here.
- Multi-client concurrent editing of settings. The broker-authoritative model accommodates it later without redesign.
- A user-facing widget customization or theming system beyond what `View::Style` already encodes.
- Pricing data, telemetry settings, keybinding customization. Future categories that this design makes cheap to add but does not implement.
- A user-customizable command DSL. Bindings are data-shaped (forward-compat); commands are Rust functions in v1. A future v2 expression DSL is its own design.
- Cross-process subscriptions, transactional multi-path writes, persistent/replayable subscription history.

## 3. The pattern — path-based MVU

This pattern is *The Elm Architecture with StructFS as the Model*. There is no per-screen state machine, no router, no page enum, no command taxonomy invented for the screen. The substrate is the namespace.

### 3.1 Three trees

| Tree | What | Lifetime | Where it lives |
|------|------|----------|----------------|
| **Data tree** | Authoritative state | Persistent (or session-bound) | StructFS storage (`config/...`, `threads/...`) |
| **Display tree** | Where the user is right now | Same lifetime as the session | StructFS namespace (`settings/index`, `settings/accounts/_detail`) |
| **View tree** | What the screen looks like this frame | One frame, derived | In-memory `View` value, ephemeral |

The three are independent and serve different purposes. Renderers consume the data tree, are dispatched by the cursor over the display tree, and emit values in the view tree. The view tree is rendered to the terminal by a separate translator (the only place that touches ratatui).

Most prior systems collapse two of these. Web SPAs collapse data and display (URL is part of the model). Most TUI libraries collapse display and view (the renderer mutates the frame directly). MVU separates data from display+view; this design separates all three.

### 3.2 The loop

| TEA term | This system |
|----------|-------------|
| Model | StructFS namespace (the substrate; never wrapped) |
| View | `Renderer::render(&Reader) -> View` |
| Update | `Command::run(&Reader) -> Vec<Write>` |
| Subscriptions | `Subscription::handle(&Reader, change) -> Vec<Write>` |
| Cmd | (Long-running effects spawn through subscriptions) |

The cursor (`ui/<screen>/cursor: Path`) selects which renderer runs each frame. User keystrokes look up a command by `(screen, cursor, key)` and invoke `Command::run(&snapshot)` — pure function, returns `Vec<Write>`. Writes go to StructFS through the dispatching store. Subscriptions watch path patterns; when a watched path changes, the matching subscription's `handle` produces more writes (and may spawn async work). Renderers re-run on the next frame against the new snapshot.

Concrete event-loop shape:

```rust
loop {
    let snap = fetch_settings_view_state(&client).await;       // async (between frames)
    let cursor = snap.read_typed(&path!("ui/settings/cursor"))?;
    let view: View = registry.render(&cursor, &snap);          // sync, pure
    view::render_to_frame(&view, &mut terminal)?;              // sync, pure translation
    let key = next_key().await;                                // async
    let writes = dispatch(&mut snap, screen, &cursor, key, &cmds, &bindings);
    for w in writes { client.write(&w.path, w.record).await?; }
}
```

**Pre-fetch budget.** v1 assumes the renderer's read set is ≤ O(1k) records and refetches the relevant subtree between frames. The settings screen is well within that — accounts × catalog entries × UI pointers totals to a few hundred. When a future screen exceeds the budget, the path forward is subscription-driven incremental snapshots: the snapshot mutates in place when watched paths change, and the renderer is invoked with the same `&mut dyn Reader` interface. The View enum's structural diffability supports incremental rendering atop that. Both are forward-compatible with v1 without changing renderer signatures.

### 3.3 The subscription protocol (over StructFS)

StructFS itself provides only reads and writes. **Subscriptions are a protocol layered above** — a small runtime in the broker that intercepts writes, looks up matching subscriptions, and invokes them. Outside the broker (e.g. a `LocalConfig` snapshot in a unit test) the protocol is absent and the substrate works fine without it.

```rust
pub trait Subscription: Send + Sync {
    fn id(&self) -> &SubscriptionId;
    fn watches(&self) -> &[PathPattern];

    /// Called synchronously after the watched write commits. Returns
    /// additional writes to be applied (cascade-bound). The reader handed to
    /// the handler is *live* (not pinned to the post-write state) — successive
    /// reads inside one handler may observe concurrent writes; handlers that
    /// reason about cross-path consistency must coordinate themselves.
    /// May also spawn long-running work via ctx.spawn; spawned tasks write
    /// back through ctx.writer. Errors and panics are caught at the
    /// dispatcher boundary.
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write>;
}

pub struct SubCtx<'a> {
    pub snapshot: &'a mut dyn Reader,    // live broker reader (NOT pinned)
    pub change:   &'a PathChange,
    pub spawn:    &'a dyn SpawnHandle,   // for long-running tasks
    pub writer:   Arc<dyn AsyncWriter>,  // back-channel for spawned tasks
}

pub enum PathPattern {
    Exact(Path),
    Prefix(Path),                                // matches anything under this prefix
    /// Matches paths whose components start with `prefix`'s components
    /// AND end with `suffix`'s components, with **one or more** components
    /// in between. Use case: per-instance action paths like
    /// `config/gate/accounts/{name}/test_now` —
    /// prefix `config/gate/accounts`, suffix `test_now`.
    PrefixSuffix { prefix: Path, suffix: Path },
}
```

**Permissiveness of `PrefixSuffix`.** The "one or more middle components" semantics means `PrefixSuffix { prefix: config/gate/accounts, suffix: test_now }` also matches `config/gate/accounts/foo/bar/test_now` — a hypothetical sub-resource under an account. This is fine today (no nested account-sub-resources exist) but the pattern's *correctness* depends on that staying true. If a future "sub-resource of an account" lands (e.g. `config/gate/accounts/{name}/scoped_keys/{key}/test_now`), it will need a sharper pattern variant — `Template { segments: Vec<TemplateSegment> }` with `Wildcard` at specific positions. Flagging the assumption now so the next maintainer doesn't trip on it.

```rust

pub struct PathChange {
    pub path:   Path,
    pub before: Option<Record>,
    pub after:  Option<Record>,    // None = deletion
}

pub trait SpawnHandle: Send + Sync {
    fn spawn(&self, task: BoxFuture<'static, ()>) -> AbortHandle;
}

pub trait AsyncWriter: Send + Sync {
    fn write<'a>(&'a self, path: &'a Path, record: Record)
        -> BoxFuture<'a, Result<Path, StoreError>>;
}
```

Runtime contract (broker-side):

1. At startup, every Subscription is registered against the broker (`Broker::register_subscription`). The runtime indexes them by `PathPattern`.
2. On every successful write, the runtime computes the `PathChange`, looks up matching subscriptions, and calls each one's `handle` with a *live* broker reader (not a pinned snapshot — the broker has no global version to pin against). Successive reads inside a handler may observe writes that landed after the trigger; handlers reading multiple paths and reasoning about cross-path consistency must coordinate themselves (e.g. read everything they need into local variables before any await). Writes returned from `handle` are queued and applied through the same dispatcher (re-triggering subscriptions, fixpoint, with a cascade depth bound — default 64). Each matching subscription is invoked exactly once per triggering write; if a subscription's `watches()` lists multiple patterns that overlap on the triggering path, the dispatcher dedups so the handler doesn't fire multiple times. Authors can think of `watches()` as a union.
3. Subscriptions whose `handle` spawns long-running async tasks return immediately after spawning. Spawned tasks write back through `Arc<dyn AsyncWriter>`. Subsequent writes to the same trigger path can abort prior tasks via an `AbortHandle` the subscription holds.
4. A subscription handler that panics or returns an error is contained: log via `tracing::error!` with the subscription id; siblings still run; original `write()` returns Ok.
5. **Ordering across multiple subscriptions on a single write.** When multiple subscriptions match the same write, they fire in registration order; their returned writes are queued FIFO and applied in that order. Authors who care about ordering control it through registration sequence at startup.

**Runtime requirement.** The dispatcher's production `SnapshotReader` bridges sync `Reader::read` calls to async broker reads via `tokio::task::block_in_place`, which requires a multi-threaded tokio runtime. Callers on a `current_thread` runtime will panic on the first triggered subscription. The fast-path that skips snapshot reads when no subscription matches keeps `current_thread` callers working in the no-listener case (subscriptions only kick in when a registered listener actually exists). v1 ships multi-threaded; a future single-threaded variant would need an async `Reader` trait or a different bridging strategy.

**Fast-path.** When no registered subscription matches the written path, the dispatcher skips the snapshot read and returns immediately after the substrate write. This bounds the no-listener case at one substrate write per call (no extra round-trips) and keeps `current_thread` callers functional in the no-subscription case (the `block_in_place` bridge is only entered when a handler will actually run). The fast-path is a load-bearing design property, not an incidental optimization — implementations that re-derive this dispatcher should preserve it.

Subscriptions subsume:

- Long-running actions: `test_now → test_status` is a subscription on `PrefixSuffix { prefix: config/gate/accounts, suffix: test_now }` whose `handle` writes `Testing { started_at_ms }` synchronously and spawns a task that writes `Success`/`Failed` later.
- Validation at action time: same subscription computes diagnostics first, writes `validation_status`, short-circuits on errors.
- Catalog write-back on refresh.
- Multi-write orchestration: deletion handler removes record + key + provider entry + clears selection in one synchronous handler call.

There is no separate "action handler" type. There are subscriptions whose handlers happen to perform long async work. The `*_now`/`*_status` convention is a *naming convention* the protocol formalizes.

### 3.4 Identifiers are values; path components are literals

Identifiers from outside our control (model ids from server catalogs, anything user-typed-and-not-yet-validated) are **values**, never path components. Internally controlled, validated, dot-free identifiers (command ids, sentinel labels) can be path components. Account names sit on the line — user-chosen but validated by `PathComponent::try_new` at the input boundary, so existing `config/gate/accounts/{name}/...` storage stays as-is.

Consequence for the display tree: cursors don't carry captures. Pages with instance state use a fixed sentinel cursor (`settings/accounts/_detail`) plus a separate selection pointer (`ui/settings/accounts/selected`) that holds the identifier as a typed value.

### 3.5 Typed across every boundary

Everything that crosses the broker boundary is `serde`-serialized but statically typed at both ends. Producer and consumer both know the type; serde mediates the wire form. The compiler enforces shape at every read/write site. Adding a new record kind requires Rust changes; adding a new instance of an existing record kind is a typed write to a path.

Renderers consume `&mut dyn Reader` and emit `View` — both typed. Commands consume `&mut dyn Reader` and emit `Vec<Write>` — both typed. Subscriptions consume snapshot+change and emit writes — same. There is no untyped intermediate stage.

### 3.6 Why this works in this codebase

- Reads cascade through overlays via `Cascade<A, B>` (`crates/ox-store-util/src/cascade.rs`). Same renderer transparently shows base or overlay-cascaded state.
- `oxpath!` produces typed `Path` values with validated components.
- Typed records via `read_typed::<T>` / `write_typed` already span the broker boundary; this design adds new typed records but no new mechanism.
- Recent commit `ed6d304 ("broker-authoritative focus + inline approval card")` is the codebase already moving in this direction.

## 4. Architecture (for the settings screen)

### 4.1 Cursor

The cursor lives at `ui/settings/cursor` and holds a `Path`. Initial state: `oxpath!("settings", "index")`.

Movement happens via three commands:

- `nav.descend.<area>` — writes the highlighted entry's `target_cursor` to `ui/settings/cursor`.
- `nav.ascend` — applies the renderer's `AscendRule` to compute the parent display-tree path and writes it.
- `highlight.<area>.{next,prev}` — write to the appropriate per-area selection pointer; cursor unchanged.

```rust
pub enum AscendRule {
    /// Walk the display-tree parent chain until a registered renderer matches.
    NearestRegistered,
    /// Top-level page within a screen: ascend to the named cursor (typically
    /// the screen's index page). The named target must be a registered cursor.
    Fallback(Path),
    /// Top-level page; ascending exits the settings screen entirely.
    ExitScreen,
}
```

`settings/index` uses `ExitScreen`. Top-level pages (`settings/accounts`, `settings/models`) use `Fallback(settings/index)`. Detail pages and overlays use `NearestRegistered`.

### 4.2 Selection pointers

Pages with instance state use per-area typed pointers:

| Path                                | Type                  | Meaning                                                              |
|-------------------------------------|-----------------------|----------------------------------------------------------------------|
| `ui/settings/index/selected`        | `usize`               | Highlighted row on the index.                                         |
| `ui/settings/accounts/selected`     | `Option<String>`      | Account name highlighted on Accounts list / being edited on Detail.   |
| `ui/settings/account_detail/field`  | `AccountField`        | Focused form field on Account Detail.                                 |
| `ui/settings/models/selected`       | `Option<ModelKey>`    | Highlighted (account, model) row.                                     |
| `ui/settings/model_detail/field`    | `ModelField`          | Focused form field on Model Detail.                                   |
| `ui/settings/edit_cursor`           | `u32`                 | Character cursor inside the focused text field.                        |

Each renderer reads only the pointers it needs.

### 4.3 Renderers and the View tree

A renderer is a *pure function* from a Reader to a View. It cannot draw, await, mutate, or have side effects.

```rust
pub trait Renderer: Send + Sync {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View;
    fn ascend_to(&self) -> AscendRule;
}

pub struct RenderCtx<'a> {
    pub area:     Rect,
    pub data:     &'a mut dyn Reader,
    pub registry: &'a RendererRegistry,
    pub theme:    &'a Theme,
}

pub struct RendererRegistry { specs: HashMap<Path, Box<dyn Renderer>> }

impl RendererRegistry {
    pub fn render(&self, cursor: &Path, ctx: &mut RenderCtx<'_>) -> View {
        match self.specs.get(cursor) {
            Some(r) => r.render(ctx),
            None    => View::unknown_cursor_fallback(cursor),
        }
    }
    pub fn ascend(&self, cursor: &Path) -> Option<Path> { /* …NearestRegistered walk */ }
}
```

> Reader signatures are `&mut dyn Reader` throughout (Renderer, Command, RenderCtx, CommandCtx, SubCtx, dispatch, the event-loop snippet). `Reader::read` is `&mut self` because production Reader implementations (`LiveReader`, `LocalConfig`) hold lazy decode caches — the mutation is internal to the Reader, not to observable application state. Renderers and commands remain pure with respect to the namespace.

**Composition is value-shaped, not call-shaped.** A modal-over-page renderer constructs its View from sub-Views by calling the registry recursively and wrapping:

```rust
fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
    let parent = ctx.registry.render(&oxpath!("settings","accounts"), ctx);
    let modal  = self.render_modal_body(ctx);
    View::Modal { background: Box::new(parent), foreground: Box::new(modal), dim: true }
}
```

The translator (`view::render_to_frame(&View, &mut Frame, Rect)`) is the only place ratatui is touched. It is total over the View enum and pure: same View → same Frame contents.

### 4.4 Commands as Rust trait impls

A command is a pure function from snapshot to writes. Built-in commands are Rust types implementing `Command`; the registry stores trait objects keyed by `CommandId`.

```rust
pub trait Command: Send + Sync {
    fn id(&self)      -> &CommandId;
    fn display(&self) -> &CommandDisplay;
    fn scope(&self)   -> &CommandScope;
    fn run(&self, snapshot: &mut dyn Reader, ctx: &CommandCtx<'_>) -> Vec<Write>;
}

/// Non-data services a command may legitimately need.
/// Kept narrow on purpose; growing this is a deliberate language extension.
pub struct CommandCtx<'a> {
    pub registry:        &'a RendererRegistry,   // for nav.ascend's parent walk
    pub last_keystroke:  Option<KeyChord>,       // for field.insert / field.delete_back
}

pub struct Write { pub path: Path, pub record: Record }
pub struct CommandRegistry { by_id: HashMap<CommandId, Box<dyn Command>> }
```

There is **no on-the-wire effect DSL.** No `PathTemplate`, no `PayloadSource`, no `CommandEffect`.

**What goes where.** Commands take inputs from three places, by category:

- **Data inputs** → `&mut dyn Reader`. Anything stored in the namespace: selection pointers, draft text, focused-field pointers, all of `config/*`, all of `ui/*`.
- **Per-invocation non-data inputs** → `CommandCtx`. Things the dispatcher knows that vary per dispatch and aren't namespace-shaped: the renderer registry (a static structural reference, not data), the just-pressed key.
- **Ambient services** → captured at command construction. Transport clients, config dirs, anything resolved once at startup. Subscriptions follow the same rule.

`CommandCtx`'s growth bound is the second category alone. New per-dispatch non-data inputs (a future "search query buffer," a "modifier register") may join. New ambient services do not — they're closed-over at registration. New data inputs do not — they go in the namespace.

A command that "sets primary" reads `ui/settings/models/selected: ModelKey`, builds a `CompletionRole`, returns one write to `config/gate/completions/primary`. Eight lines of Rust.

The `j`/`k` keystroke is a `HighlightArea(Area::Accounts)` command whose `run` reads the current selection pointer and the live row count, computes the next index, returns one write. No broker round-trip; no path-template DSL.

`NavAscend` reads the cursor from the snapshot and consults `ctx.registry.ascend(&cursor)` to compute the parent. `FieldInsert` reads the focused field's content from the snapshot and `ctx.last_keystroke` for the character to insert. Both stay in the regular dispatch path; no special cases in the dispatcher.

**Bindings remain data-shaped** (`BindingEntry`): they carry a `command_id`, the registry resolves it to a trait object, `run` produces writes. Forward-compat to user-customizable bindings; *not* forward-compat to user-customizable commands (those would need a v2 expression DSL — explicitly deferred).

### 4.5 Bindings

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BindingEntry {
    pub screen:      Screen,
    pub cursor_path: Option<Path>,    // None = whole-screen scope; Some = exact match
    pub mode:        Option<Mode>,
    pub key:         KeyChord,
    pub command_id:  CommandId,
}
```

Lookup specificity (most → least): cursor_path-Some + mode-Some > cursor_path-Some + mode-None > cursor_path-None + mode-Some > cursor_path-None + mode-None. Ties broken by registration order. The registry sorts at startup; lookup is a linear scan.

`KeyChord` is a typed struct (modifier set + key code) — promoted from today's stringly-typed `key_str`.

### 4.6 Action paths via subscriptions

The `*_now` / `*_status` convention is implemented as subscriptions:

| Watched pattern                                                                          | Subscription                  | Effects                                                                                  |
|------------------------------------------------------------------------------------------|-------------------------------|------------------------------------------------------------------------------------------|
| `PrefixSuffix { prefix: config/gate/accounts, suffix: test_now }`                        | `AccountTestSubscription`     | Validates synchronously; spawns test; writes `…/test_status` progression                 |
| `PrefixSuffix { prefix: config/gate/accounts, suffix: refresh_now }`                     | `CatalogRefreshSubscription`  | Validates; fetches catalog; writes `models` + `refresh_status`                           |
| `PrefixSuffix { prefix: config/gate/accounts, suffix: delete_now }`                      | `AccountDeleteSubscription`   | Removes account record + key + provider entry; clears selection; pops cursor             |
| `Exact(config/gate/accounts/_create_now)`                                                | `AccountCreateSubscription`   | Validates name; writes default `AccountConfig`; sets selection + cursor                  |
| `Exact(config/save)`                                                                     | `ConfigSaveSubscription`      | Persists `config/*` to disk                                                               |

**Path convention.** Per-instance actions live at `<collection>/{id}/<verb>_now`. Collection-level actions live at `<collection>/_<verb>_now` — a sentinel sibling under the collection. The leading `_` distinguishes sentinels from user identifiers (which are `PathComponent::try_new`-validated and never start with `_`). This convention applies uniformly: `config/gate/accounts/_create_now` for collection create; `config/gate/accounts/{name}/test_now` for per-instance test. Renderers and subscriptions both rely on the convention; new collection-level actions follow it.

**Supersession.** `AccountTestSubscription` and `CatalogRefreshSubscription` hold a `Mutex<HashMap<String, AbortHandle>>` keyed by account name; a new write to the same trigger path aborts the prior task and starts fresh. Cancellation is structural — superseded tasks are dead and cannot write stale results.

**Why orchestration moves to subscriptions.** Several settings actions need *multiple* coordinated writes (create-account: record + selection + cursor; delete-account: removal + selection clear + cursor pop). Commands stay narrow (pure function, snapshot in, writes out). Multi-write orchestration in response to a write lives in the subscription, which knows resource semantics, runs writes in invariant-preserving order, and emits status atomically with side effects.

## 5. Data shapes

All cross-boundary records are `serde`-derived. No `#[serde(default)]` for backward compatibility — we don't carry compat.

### 5.1 `ModelInfo` (extended; lives in `ox-types`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id:                 String,
    pub display_name:       String,
    pub max_context_size:   Option<u32>,
    pub max_output_tokens:  Option<u32>,
    pub source:             ModelInfoSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelInfoSource { Server, KnownTable, UserOverride }
```

Resolution order at request time: codec-fetch → known-family table → user-override.

- `max_context_size` — input ceiling. Operationally meaningful for an agent harness.
- `max_output_tokens` — wire-required output cap; sent as the request's `max_tokens`.

### 5.2 Catalog storage

`config/gate/accounts/{name}/models` holds a single `Vec<ModelInfo>` record per account. Independent per account because (endpoint, auth-key, dialect) varies.

### 5.3 `CompletionRole` (in `ox-types`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionRole {
    pub account:  String,
    pub model_id: String,
}
```

Field name `model_id` matches `ModelKey.model_id` so a Models selection lifts directly into a primary tag. Stored at `config/gate/completions/{role_name}`. Day-one role: `primary`.

### 5.4 Action statuses

```rust
pub enum AccountTestStatus {
    Idle,
    Testing { started_at_ms: u64 },
    Success { dialect: String, latency_ms: u64, completed_at_ms: u64 },
    Failed  { reason: String,                   completed_at_ms: u64 },
}

pub enum CatalogRefreshStatus {
    Idle,
    Refreshing { started_at_ms: u64 },
    Success { models_added: u32, models_updated: u32, completed_at_ms: u64 },
    Failed  { reason: String,                         completed_at_ms: u64 },
}
```

### 5.5 `KnownFamilyEntry` (in `ox-gate`)

```rust
pub struct KnownFamilyEntry {
    pub max_context_size:  Option<u32>,
    pub max_output_tokens: Option<u32>,
}
pub fn known_family_metadata(model_id: &str, dialect: &str) -> Option<KnownFamilyEntry>;
```

Const slice covering Claude 3.x/4.x/4.5, GPT-4o, GPT-4 turbo, Llama 3.x.

### 5.6 UI records (in `ox-types`)

```rust
pub enum AccountField { Name, Protocol, Endpoint, Auth, Key }
pub enum ModelField   { ContextSizeOverride, OutputTokensOverride }

pub struct ModelKey { pub account: String, pub model_id: String }

pub struct SettingsIndexEntry {
    pub id:            String,
    pub label:         String,
    pub description:   String,
    pub target_cursor: Path,
    pub badge:         BadgeSource,
}

pub enum BadgeSource {
    None,
    Static(String),
    SubtreeCount(Path),
    PrimaryReference,    // resolves to "{account} / {model}" from config/gate/completions/primary
}

pub struct ValidationDiagnostics {
    pub field_errors:    BTreeMap<AccountField, String>,
    pub computed_at_ms:  u64,
}

pub enum GlobalBanner {
    None,
    Error { message: String, set_at_ms: u64 },
    Info  { message: String, set_at_ms: u64 },
}

pub struct KeyChord       { pub modifiers: KeyModifierSet, pub code: KeyCodeRepr }
pub struct KeyModifierSet { pub ctrl: bool, pub alt: bool, pub shift: bool, pub super_: bool }
pub enum KeyCodeRepr      { Char(char), Enter, Esc, Tab, BackTab, Backspace, Delete,
                            Up, Down, Left, Right, PageUp, PageDown, Home, End, Insert, F(u8) }

pub struct BindingEntry      { /* §4.5 */ }
pub struct CommandId(pub String);
pub struct CommandDisplay    { pub name: String, pub description: String }
pub struct CommandScope      { pub screen: Screen, pub cursor_path: Option<Path> }
pub struct SubscriptionId(pub String);
pub struct PathChange        { pub path: Path, pub before: Option<Record>, pub after: Option<Record> }
pub enum   PathPattern       { Exact(Path), Prefix(Path), PrefixSuffix { prefix: Path, suffix: Path } }
pub struct Write             { pub path: Path, pub record: Record }
```

### 5.7 The View tree (in new `ox-view`)

`ox-view` is a new tiny crate (no ratatui dependency) holding the view enum. The translator (a module in `ox-cli`) is the only place ratatui is touched.

```rust
pub enum View {
    Empty,
    Text  { spans: Vec<Span>, align: Align },
    Stack { dir: Direction, children: Vec<(View, Sizing)> },
    List  { title: Option<String>, items: Vec<ListItem>, selected: Option<usize> },
    Form  { title: Option<String>, rows: Vec<FormRow>, focused: Option<usize> },
    Modal { background: Box<View>, foreground: Box<View>, dim: bool },
    Banner { kind: BannerKind, content: String },
    StatusBlock { title: String, lines: Vec<StyledLine>, scroll_offset: u16 },
    Pad   { padding: Padding, child: Box<View> },
}

pub struct ListItem  { pub primary: String, pub secondary: Option<String>, pub badge: Option<String> }
pub struct FormRow   { pub label: String, pub value: FormValue, pub error: Option<String>, pub hint: Option<String> }
pub enum FormValue   { Text { value: String, cursor: u32, masked: bool },
                       Selector { options: Vec<String>, current: usize },
                       ReadOnly(String) }
pub enum BannerKind  { Info, Error }
pub struct Span      { pub text: String, pub style: Style }
pub struct StyledLine(pub Vec<Span>);
pub enum Direction   { Horizontal, Vertical }
pub enum Sizing      { Fill, Fixed(u16), Min(u16) }
pub struct Padding   { pub top: u16, pub right: u16, pub bottom: u16, pub left: u16 }
pub enum Align       { Left, Center, Right }
pub struct Style     { pub fg: Option<Color>, pub bg: Option<Color>, pub modifiers: ModifierSet }
```

This is the curated widget set. Anything outside requires extending the View enum **and** the translator. That cost is the point — it forces the design to stay coherent.

**Themes apply at render time, not at translation time.** The renderer reads `RenderCtx::theme` and emits Views with concrete colors baked into `Style`; the translator is theme-agnostic and total over `View` alone. Consequence: a theme switch requires re-rendering — acceptable because we re-render every frame anyway. Rejected alternative: `Style` as semantic tokens resolved at translation time. That would let the translator swap themes without re-rendering, but at the cost of a parallel "semantic style" vocabulary the renderer has to learn and the translator has to interpret. We took the simpler split.

`View` derives `PartialEq` for testability (struct equality as the assertion primitive).

### 5.8 Subscription protocol types (in `ox-broker`)

```rust
pub trait Subscription: Send + Sync { /* §3.3 */ }
pub trait SpawnHandle:  Send + Sync { /* §3.3 */ }
pub trait AsyncWriter:  Send + Sync { /* §3.3 */ }

pub struct SubscriptionRegistry {
    entries: Vec<(PathPattern, Arc<dyn Subscription>)>,
}

pub struct DispatchingStore {
    inner: Arc<dyn Store>,
    subs:  Arc<SubscriptionRegistry>,
    spawn: Arc<dyn SpawnHandle>,
    cascade_bound: usize,    // default 64
}
```

The broker's existing `Store` trait is unchanged. `DispatchingStore` wraps it; the broker holds a `DispatchingStore` and routes all writes through it.

### 5.8b API key storage moves into StructFS

> **Backward-compat note.** "We don't carry compat" (§5.9) applies to *config*. *Secrets* get a one-shot read of legacy on-disk key files because losing user keys is unrecoverable. This is the only such carve-out.

API keys today live as files outside StructFS, a historical seam from before StructFS was rich enough to hold them. This redesign closes the seam: keys move to `secret/keys/{account}: ApiKey` (where `ApiKey` is a thin newtype around `String`).

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey(pub String);
```

Reasons:

- `AccountDeleteSubscription` becomes a single self-contained `Vec<Write>` — removing the key is a typed delete, not a side-effect outside the namespace.
- The `Reader`-only renderer for Account Detail can read key presence/length without a parallel filesystem helper.
- A future overlay mount (`Cascade<encrypted_overlay, base>`) gives at-rest encryption uniformly, no special-casing for keys.

Persistence: `ConfigSaveSubscription` writes the `secret/*` subtree to a separate file (`keys.json`) with `chmod 0600` rather than mixing it into the main config. The split is at persistence, not at the namespace.

The legacy on-disk key files are read once at startup if `secret/keys/*` is empty — populating the namespace from disk — then ignored. No further migration.

### 5.9 What's removed

| Path / type                                                 | Replacement                                                              |
|-------------------------------------------------------------|-------------------------------------------------------------------------|
| `gate/defaults/{account, model, max_tokens}`                | `config/gate/completions/primary` (typed `CompletionRole`); `max_output_tokens` per-request from `ModelInfo` with kernel fallback |
| `gate/providers/{name}/models`                              | `config/gate/accounts/{name}/models` (per-account)                      |
| `SettingsFocus`, `SettingsState`, `WizardStep`              | Cursor + selection pointers + (wizard out of scope)                     |
| `Defaults` arm in `GateStore`                               | Removed; kernel reads from new paths                                    |
| `crate::settings_shell`, `crate::settings_state`, `crate::settings_view` | Subsumed by the new pattern                                  |
| (Implicit in v0 of this spec) `PathTemplate`, `PayloadSource`, `CommandEffect` | Replaced by `trait Command`                            |

We don't carry backward compatibility. Old paths sit orphaned in users' configs; the new code never reads them.

### 5.10 Where each type lives

| Crate         | Types                                                                                                        |
|---------------|-------------------------------------------------------------------------------------------------------------|
| `ox-kernel`   | (no settings-redesign types; reads `(account, model_id, max_output_tokens)` as primitives via paths)         |
| `ox-gate`     | `ApiKey`, `AccountTestStatus`, `CatalogRefreshStatus`, `KnownFamilyEntry`, `known_family_metadata()`, settings subscription impls, transport (relocated from `ox-cli`) |
| `ox-types`    | `AccountField`, `BadgeSource`, `BindingEntry`, `CommandDisplay`, `CommandId`, `CommandScope`, `CompletionRole`, `GlobalBanner`, `KeyChord`/`KeyModifierSet`/`KeyCodeRepr`, `ModelField`, `ModelInfo`, `ModelInfoSource`, `ModelKey`, `PathChange`, `PathPattern`, `SettingsIndexEntry`, `SubscriptionId`, `ValidationDiagnostics`, `Write` |
| `ox-view`     | `View`, `ListItem`, `FormRow`, `FormValue`, `Style`, `Span`, `Direction`, `Sizing`, `Padding`, `Align`, `BannerKind`, `Color`, `ModifierSet` |
| `ox-broker`   | `Subscription` trait, `SubscriptionRegistry`, `DispatchingStore`, `SpawnHandle`, `AsyncWriter` impls         |
| `ox-cli`      | `Renderer` trait, `RendererRegistry`, `RenderCtx`, `AscendRule`, settings renderers, `Command` trait + `CommandCtx` + impls, `CommandRegistry`, `BindingRegistry`, View→ratatui translator, snapshot builder |

_`ModelInfo` and `CompletionRole` live in `ox-types` (not `ox-gate`) so the kernel can read them without introducing a `kernel → gate` dependency cycle._

Moving `ModelInfo` out of `ox-kernel` and into `ox-types` is a one-time refactor at the start of implementation.

## 6. The settings namespace (concrete pages)

All paths use `oxpath!`. Notation `oxpath!("config","gate","accounts",name_comp,"endpoint")` is shortened to `config/gate/accounts/{name}/endpoint` for readability.

### 6.1 Index — `settings/index`

**View:** `View::List` of top-level categories with optional preview badges.
**Reads:** `settings/index/entries/*` → `Vec<SettingsIndexEntry>`; per-entry badges.
**Selection pointer:** `ui/settings/index/selected: usize`.
**Bindings:**

| Key       | Command id                  | Effect                                                                        |
|-----------|-----------------------------|-------------------------------------------------------------------------------|
| `j` / `k` | `highlight.index.next/prev` | Pure: read `selected` + entry count → write next                              |
| `Enter`   | `nav.descend.index`         | Read highlighted entry's `target_cursor` → write `ui/settings/cursor`         |
| `Esc`     | `nav.ascend`                | `AscendRule::ExitScreen` — exits the settings screen                          |

Day-one entries (registered at startup):

| id       | label    | target_cursor       | badge                                  |
|----------|----------|---------------------|----------------------------------------|
| accounts | Accounts | `settings/accounts` | `SubtreeCount(config/gate/accounts)`   |
| models   | Models   | `settings/models`   | `PrimaryReference`                     |

### 6.2 Accounts list — `settings/accounts`

**View:** `View::List` of accounts.
**Reads:** `config/gate/accounts/*`, `config/gate/providers/{ref}`, `secret/keys/{name}: ApiKey` (presence drives the `✓key`/`–` indicator).
**Selection pointer:** `ui/settings/accounts/selected: Option<String>`.

| Key       | Command id                       | Effect                                                            |
|-----------|----------------------------------|-------------------------------------------------------------------|
| `j` / `k` | `highlight.accounts.next/prev`   | Pure: cycle selection                                              |
| `Enter`   | `nav.descend.accounts`           | Write `ui/settings/cursor ← settings/accounts/_detail`             |
| `a`       | `accounts.add`                   | Write `ui/settings/cursor ← settings/accounts/_new`                |
| `d`       | `accounts.delete_confirm`        | Write `ui/settings/cursor ← settings/accounts/_delete`             |
| `Esc`     | `nav.ascend`                     | Write `ui/settings/cursor ← settings/index`                        |

### 6.3 Account detail — `settings/accounts/_detail`

**View:** `View::Stack { dir: Vertical, children: [Form, StatusBlock] }`.

If selection is `None`: empty-state pane ("No account selected. Press Esc to return."). If selection points at a removed account: same empty-state with a one-line note. The deletion subscription clears the dangling pointer atomically with the removal, so this stale state is observed at most one frame.

**Reads:** `config/gate/accounts/{selected}`, provider, `secret/keys/{selected}: ApiKey`, `…/test_status`, `…/refresh_status`, `…/validation_status`.
**Selection pointers:** `ui/settings/account_detail/field`, `ui/settings/edit_cursor`.

| Key                            | Command id                        | Effect                                                                                   |
|--------------------------------|-----------------------------------|------------------------------------------------------------------------------------------|
| `Tab`/`Down`                   | `field.account.next`              | Pure: cycle focused field                                                                 |
| `Shift+Tab`/`Up`               | `field.account.prev`              | Pure: cycle focused field                                                                 |
| `Left`/`Right` (Protocol/Auth) | `selector.cycle.<field>`          | Pure: read AccountConfig, modify selector field, write whole record                       |
| char keys (text fields)        | `field.insert`                    | Pure: read field + cursor, insert char, write field + new cursor                          |
| `Backspace`                    | `field.delete_back`               | Pure                                                                                      |
| `t`                            | `account.test`                    | Write `Null` to `…/test_now` (subscription does the work)                                 |
| `Ctrl+s`                       | `app.save`                        | Write `Null` to `config/save`                                                             |
| `Esc`                          | `nav.ascend`                      | Write `ui/settings/cursor ← settings/accounts`                                            |

Field-edit writes are per-keystroke and per-path. The next `t` test uses the latest value automatically.

### 6.4 New account overlay — `settings/accounts/_new`

**View:** `View::Modal { background: <accounts list>, foreground: <prompt form> }`.

- `Enter` with valid `PathComponent` name: writes typed `CreateAccountRequest { name }` to `config/gate/accounts/_create_now`. `AccountCreateSubscription` validates, writes default config, sets selection, pops cursor.
- `Esc`: writes `ui/settings/cursor ← settings/accounts`.

Account names are immutable post-creation in v1; rename = delete + recreate.

### 6.5 Delete account overlay — `settings/accounts/_delete`

**View:** `View::Modal { background: <accounts list>, foreground: <confirm box> }`.

- `y`: writes `Null` to `config/gate/accounts/{selected}/delete_now`. `AccountDeleteSubscription` removes record + key + provider entry, clears selection, pops cursor.
- `n`/`Esc`: writes `ui/settings/cursor ← settings/accounts`.

### 6.6 Models — `settings/models`

**View:** `View::List` of one row per `(account, model_id)`.
**Reads:** all account catalogs, `config/gate/completions/primary`, per-account `refresh_status`.
**Selection pointer:** `ui/settings/models/selected: Option<ModelKey>`.

| Key       | Command id                    | Effect                                                                                           |
|-----------|-------------------------------|--------------------------------------------------------------------------------------------------|
| `j` / `k` | `highlight.models.next/prev`  | Pure: cycle through (account, model) pairs                                                       |
| `Enter`   | `nav.descend.models`          | Write `ui/settings/cursor ← settings/models/_detail`                                              |
| `P`       | `models.set_primary`          | Pure: read `selected: ModelKey` → write `config/gate/completions/primary: CompletionRole`              |
| `r`       | `account.refresh`             | Pure read of `selected.account` → write `Null` to `…/{account}/refresh_now`                      |
| `Esc`     | `nav.ascend`                  | Write `ui/settings/cursor ← settings/index`                                                       |

### 6.7 Model detail — `settings/models/_detail`

**View:** `View::Form` with id (read-only), display name (read-only), max_context_size + source badge, max_output_tokens + source badge.
**Writes:** Override fields rewrite the whole catalog `Vec<ModelInfo>` with `source = UserOverride` for the modified entry.
**Selection pointer:** `ui/settings/model_detail/field: ModelField`.
**Bindings:** Same shape as Account detail.

### 6.8 First-run

If `config/gate/accounts` is empty at startup, the snapshot builder writes `ui/settings/cursor ← settings/accounts/_new` once before the first frame. The user lands on the new-account overlay over an empty Accounts list, with a clear next step. No wizard scaffolding needed; the regular pages do the job.

### 6.9 Deferred

Appearance, Keybindings, About — placeholders.

## 7. Error handling

Errors flow through the namespace as typed values. Renderers pattern-match on `Result`-shaped or status-enum-shaped reads.

### 7.1 Validation

Field writes accept any value; validation runs at action time, not per keystroke. The action's subscription writes `validation_status` (`ValidationDiagnostics`) synchronously before any wire activity; if `field_errors` is non-empty, the action short-circuits and writes `Failed { reason: "validation failed" }` to its status path.

Existing helpers: `ox_gate::validate_endpoint`, `ox_gate::AuthScheme::requires_key`, `PathComponent::try_new`. Compose into the validation pass.

### 7.2 Action failures

Typed via `*Status::Failed { reason }`. Status block in the View is scrollable for multi-line errors.

### 7.3 Missing data

| Condition                                                         | Renderer behavior                                                                                    |
|-------------------------------------------------------------------|------------------------------------------------------------------------------------------------------|
| Cursor at `…/_detail`, selection unset                            | `View` empty-state: "No account selected. Press Esc to return."                                       |
| Selection points at non-existent account                          | Same empty-state. The deletion subscription clears the dangling pointer atomically with the removal.  |
| Cursor at unregistered path                                       | `View::unknown_cursor_fallback(cursor)`: "Unknown settings location. Esc to return to index."         |

### 7.4 Broker errors

Reads → `None`, logged via `tracing::error!`. Writes → logged; banner via `ui/global/banner` (auto-clears after ~5s).

### 7.5 Schema/type errors

Debug builds: panic with path + deserialize error.
Release builds: log, treat read as `None`, continue.
Boundary: panic only at sites we own; user-config deserialize is fault-tolerant.

### 7.6 Subscription failures

A subscription handler panic is contained: log via `tracing::error!` with subscription id; sibling subscriptions still run; original `write()` returns Ok. A subscription-produced write that fails: log; cascade for that branch ends; siblings continue.

### 7.7 Subscription cascades

The runtime bounds write→subscription→write chains at 64 steps **per causal frame**. A "causal frame" is one synchronous descent: the original `write()` and the cascade of synchronous handler returns underneath it. Spawned tasks open a *new* causal frame when they write back through the dispatcher — the bound resets to 0 for each spawned write. Well-behaved subscriptions converge in 1–2 steps.

**Known property, not known-good property:** a pathological pair of subscriptions where each spawns a task that writes to the other's watched path can loop indefinitely, bounded only by tokio task scheduling. v1 ships without protection against this; v2 will introduce a context-carried trigger generation id that the back-channel `AsyncWriter` honors, capping causal-chain length even across spawn boundaries. None of the day-one settings subscriptions exhibit this shape.

## 8. Testing strategy

### 8.1 Typed records (serde round-trip)

Each typed record has a small round-trip test. ~15 small tests covering the records in §5.

### 8.2 Pure helpers

- `known_family_metadata(model_id, dialect)` — table lookup.
- `AscendRule` resolution — parent walking.
- `View::unknown_cursor_fallback` — fallback construction.
- `KeyChord` parsing/matching.
- `PathPattern::matches(path)` — Exact/Prefix semantics, boundary cases.

### 8.3 Renderer tests (View struct comparison)

Each renderer takes `&RenderCtx` and returns a `View`. Tests:

1. Build a `LocalConfig` Reader populated with synthesized records.
2. Call `renderer.render(&ctx)` → returns `View`.
3. Compare `View` for equality directly (`assert_eq!(actual, expected)`) or use `insta::assert_yaml_snapshot!` of the View value.

Snapshots over `View` structs are stable across terminal width changes, theme tweaks, and translator refactors. ~25 snapshots; minimal layout-dependent fragility.

### 8.4 Translator tests

The View → ratatui translator is tested with 5-8 buffer-snapshot tests covering one example of each `View` variant. Locks down layout but is decoupled from per-page content.

### 8.5 Command tests

Each `Command::run(&snapshot) → Vec<Write>` is a pure function. Set up snapshot, call run, assert writes. ~25 tests for day-one set; one per command, plus inert cases.

### 8.6 Subscription tests

Each `Subscription::handle` is testable with a `MockSpawn` (records spawned futures) and `MockWriter` (records writes). For async work, a transport trait + mock. Cover: trigger → status progression → final state; supersession; failure paths.

### 8.7 Subscription protocol tests

Independent of consumers:

- Cascade depth bound (subscriber writes to a path it watches → bound triggers, error logged).
- Panic isolation (one subscriber panics → siblings still run; original write Ok).
- Write ordering (subscriber returns `[A, B]` → applied in order).
- Spawn lifecycle (spawned task survives `write()` return; later writes land and trigger their own subscribers).
- Pattern matching (Exact, Prefix; boundary cases).

~10 tests in `ox-broker`.

### 8.8 End-to-end (headless)

A small harness:

1. Spins up a broker with `LocalConfig` mounts and the subscription runtime.
2. Populates initial state.
3. Drives keystrokes through the full dispatch (keystroke → command → writes → subscriptions → rendered View).
4. Asserts namespace state and final View structure.

3-5 scenarios covering principal happy paths.

### 8.9 What we explicitly don't test

- Pixel-perfect terminal output.
- Real provider network calls (covered by transport mocks).
- Ratatui itself.

## 9. Why not?

### 9.1 Why not Redux-style typed actions?

They'd duplicate path semantics. Paths are the action vocabulary; an "action" here is structurally a write to a specific path with a specific value. Adding a parallel typed-action layer adds a lookup table without removing path-shaped writes — and creates a second namespace where bugs and misalignment hide.

### 9.2 Why not signals/observables?

Subscriptions over paths are the same idea, indexed by namespace location instead of object identity. Identity-based subscription doesn't survive the broker boundary; path-based does. Across processes, namespace is the only thing two ends agree on.

### 9.3 Why not a single immutable Model?

Multi-mount, multi-process. The filesystem *is* the immutable model; `Cascade<A,B>` composes mounts. Wrapping all of StructFS in one Rust struct collapses the abstraction the broker is built around and makes the overlay pattern impossible.

### 9.4 Why not back-stack-based navigation?

Cursor as `Path` is canonical state; back-stack is derived state pretending to be primary. Walking parents via `AscendRule` is hierarchy-respecting and survives deep-link entry — descending then ascending lands where you'd expect even if you arrived sideways. A back-stack loses on the deep-link case.

### 9.5 Why not a data-shaped command effect language (`PathTemplate`, `PayloadSource`)?

v0 of this spec proposed it. Its inability to express field extraction, the temptation to invent `selected_account` sidecars, and the further temptation to make `j`/`k` round-trip the broker all signaled it was the wrong layer. A Rust `trait Command` covers v1 cleanly. Forward-compat to user-defined commands wants a different shape entirely (a small expression DSL in v2, defined separately, not retrofit into command effects).

### 9.6 Why not let renderers draw to ratatui directly?

Renderers as `&Reader -> View` enforce purity, get free testability (struct equality), survive a future widget-record serialization story, and constrain the design's surface to a curated widget set. `Box<dyn Fn(&mut Frame)>` was an escape hatch the size of ratatui. Closing it forces the design to stay coherent — when a future page wants something not in the View enum, the cost is a deliberate language extension, not a private detour.

### 9.7 Why subscriptions as a protocol, not a StructFS primitive?

StructFS is the storage. Subscriptions are an interpretation — "when the value here changes, do that." Pushing them into StructFS would mix storage with reactivity. Keeping them separate means a non-broker Reader (e.g. a `LocalConfig` snapshot in a unit test) doesn't need a subscription runtime to function; the protocol activates only inside the broker process. It also means subscription policy (cascade bounds, supersession, ordering) is centralized in one place rather than scattered through the storage layer. v1 also doesn't model snapshot pinning: handlers see a live reader. Strict pinning would require either MVCC in the substrate or a per-handler clone; both are v2 design questions.

### 9.8 Why are renderers re-run every frame instead of incrementally?

Pre-fetched snapshot + sync render is honest about staleness (no spooky differences between what the renderer "thinks" is true and what storage says) and trivially correct. Incremental rendering is a future optimization that this design supports — the View enum is structurally diff-able — but doesn't commit to. v1 is well within budget at the snapshot scale we expect (≤ a few thousand records).

### 9.9 Why not let commands return futures?

Commands are pure functions: `&Reader -> Vec<Write>`. Async work is for subscriptions. Keeping commands pure makes them composable, testable, and re-orderable; if a command needed to await, that's an action and belongs in a subscription. The clean separation pays for itself.

### 9.10 Why no `CommandEffect::Sequence` for multi-write commands?

Multiple writes from one keystroke are fine — `Command::run` returns `Vec<Write>`. What's banned is *orchestration* (write A, observe its effect, then write B). That's a subscription. The discipline of "commands compute a static set of writes from a snapshot" prevents the slide into a second event-loop hidden inside command execution.

### 9.11 Why two writer types in the subscription protocol (`Store` for sync, `AsyncWriter` for back-channel)?

Subscribers can't carry an `&dyn Store` across an async boundary easily; `Arc<dyn AsyncWriter>` is `Send + Sync` and clonable. They're the same underlying store; the trait split is for ergonomics around spawned tasks.

### 9.12 Why three `AscendRule` variants instead of two?

v0 of this design had two — `NearestRegistered` (strict-ancestor walk) and `ExitScreen`. Top-level pages fell into a gap: their parent in the display tree is the screen's index, but the index isn't an ancestor of `settings/accounts` (they're siblings under `settings/`). The `AscendRule::Fallback(Path)` variant lets the renderer declare its ascent target explicitly, keeping the routing decision in the renderer where it belongs rather than in `NavAscend`'s body.

## 10. Implementation sketch

Detailed plan: `docs/superpowers/plans/2026-04-27-settings-screen-redesign.md`.

1. **Type relocation.** Move `ModelInfo` from `ox-kernel` to `ox-types` (kernel needs to read it without depending on `ox-gate`).
2. **Add new typed records** in `ox-gate` and `ox-types`.
3. **Add `ox-view` crate** with the `View` enum and supporting types.
4. **Add View → ratatui translator** in `ox-cli`.
5. **Subscription protocol.** `trait Subscription`, `SubscriptionRegistry`, `DispatchingStore`, `SpawnHandle`, `AsyncWriter` in `ox-broker`. Wire into the broker's write path.
6. **Renderer registry primitive.** `trait Renderer`, `RendererRegistry`, `RenderCtx`, `AscendRule`.
7. **Command + Binding registries.** `trait Command`, `CommandRegistry`, `BindingRegistry`.
8. **Snapshot builder.** `fetch_settings_view_state`.
9. **Settings renderers** (Index, Accounts, AccountDetail, Models, ModelDetail) — return `View`.
10. **Day-one commands and bindings** — Rust `impl Command` blocks; `BindingEntry` constants.
11. **Day-one subscriptions** — test, refresh, delete, create, save in `ox-gate`. Transport relocates from `ox-cli` to `ox-gate`.
12. **Kernel resolution path.** Update `read_model_config` to use `config/gate/completions/primary` + per-account catalog.
13. **Wire dispatch and remove the bypass.** Settings flows through the regular binding mechanism.
14. **Index entries population at startup.**
15. **End-to-end integration tests.**
16. **Manual smoke + cleanup.**

## 11. Out of scope

- **First-run wizard.** First-run lands the user on `settings/accounts/_new` over an empty list; the regular pages do the job. No separate wizard module in v1.
- **Per-thread completion overrides.** Forward-compatible: the kernel reads `config/gate/completions/primary` through the thread's `Cascade<thread_overlay, base>` mount.
- **User-customizable bindings.** `BindingEntry` is data-shaped; v1 registers built-ins from Rust constants. A future feature reads/writes the same records to namespace paths.
- **User-customizable commands.** Requires a v2 expression DSL above `Command`. Separate design.
- **Widget-level Rio (View as namespace records).** `View` is in-memory only in v1; a future evolution serializes View to namespace paths and ships a generic interpreter. The View enum's shape is forward-compatible.
- **Multi-client concurrent settings editing.** Broker-authoritative state model accommodates it later.
- **Cross-process subscriptions.** Single-process v1. When multi-process arrives, subscriptions become a wire protocol — different design.
- **Transactional multi-path writes / CAS.** Out of scope. Subscribers operate on serialized writes; that's enough for v1.
- **Persistence/replay of subscriptions.** Subscriptions are runtime-only; not stored, not replayed across restarts.
- **No backward compatibility.** Old `gate/defaults/*` and `gate/providers/{name}/models` paths sit orphaned; the new code never reads them.
