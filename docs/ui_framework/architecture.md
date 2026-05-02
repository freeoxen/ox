# UI framework — architecture

How the pieces fit together. Read this once to get the mental model;
after that, work from `howto.md`.

## Three trees

The framework runs over three logically distinct trees, all keyed by
`Path`:

- **Data tree** — broker mounts (`config/`, `secret/`, `ui/`,
  `settings/`). Persistent + ephemeral facts: AccountConfig, ApiKey,
  validation_status, the `_request_exit` signal.
- **Display tree** — inferred from the *cursor* + the data tree. What
  the user is looking at right now: which page, which field is
  focused, which row is selected. Not a separate datastructure;
  selection pointers and focus indices are stored as data, the
  renderer reads them and emits the View.
- **View tree** — constructed each frame by renderers. A curated
  `View` enum. In-memory only; no serde, no ratatui.

## The cursor

`ui/settings/cursor: Path` holds the path of the page the user is
currently viewing. Default: `oxpath!("settings", "index")`.

Cursor moves are commands like everything else:

- `nav.descend.<area>` — writes the highlighted entry's
  `target_cursor` to the cursor path.
- `nav.ascend` — consults the renderer's `AscendRule` and writes the
  parent (or `_request_exit: true` for top-level pages).
- `highlight.<area>.{next,prev}` — write to per-area selection
  pointers; cursor unchanged.

`AscendRule`:

```rust
pub enum AscendRule {
    /// Walk strict ancestors until a registered renderer matches.
    /// Used by detail/list pages: Esc returns to the nearest
    /// registered ancestor.
    NearestRegistered,
    /// Top-level page; ascending exits the screen.
    /// Used by `settings/index`.
    ExitScreen,
}
```

`NavAscend` resolves in three steps:

1. Registry-defined parent (`AscendRule::NearestRegistered`).
2. Top-level fallback to `settings/index` (so Esc on
   `settings/accounts` lands there, not at `_request_exit`).
3. From the index itself, write `ui/settings/_request_exit: true`.
   The event loop reads that next frame and switches screens.

## Renderers

A renderer is a pure function from a `Reader` to a `View`. It cannot
draw, await, or mutate.

```rust
pub trait Renderer: Send + Sync {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View;
    fn ascend_to(&self) -> AscendRule;
}

pub struct RenderCtx<'a> {
    pub area:     Rect,
    pub data:     &'a mut dyn Reader,   // &mut: Reader::read takes &mut
    pub registry: &'a RendererRegistry,
    pub theme:    &'a Theme,
}

pub struct RendererRegistry {
    specs: HashMap<Path, Box<dyn Renderer>>,
}
```

The registry maps cursor paths to renderers. Lookup is exact; misses
fall back to `View::unknown_cursor_fallback(cursor)`, so the screen
never panics.

**Composition is value-shaped, not call-shaped.** A modal-over-page
renderer recurses into the registry to build the background View,
then wraps it:

```rust
fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
    let bg = ctx.registry.render(
        &oxpath!("settings", "accounts"),
        ctx,
    );
    let fg = self.render_modal_body(ctx);
    View::Modal {
        background: Box::new(bg),
        foreground: Box::new(fg),
        dim: true,
    }
}
```

## The View tree

Lives in `crates/ox-view/src/lib.rs`. **No dependencies on `serde` or
`ratatui`** — the crate is intentionally minimal. View is in-memory
only in v1.

```rust
pub enum View {
    Empty,
    Text  { spans: Vec<Span>, align: Align },
    Stack { dir: Direction, children: Vec<(View, Sizing)> },
    List  {
        title: Option<String>,
        items: Vec<ListItem>,
        selected: Option<usize>,
    },
    Form  {
        title: Option<String>,
        rows: Vec<FormRow>,
        focused: Option<usize>,
    },
    Modal {
        background: Box<View>,
        foreground: Box<View>,
        dim: bool,
    },
    Banner { kind: BannerKind, content: String },
    StatusBlock {
        title: String,
        lines: Vec<StyledLine>,
        scroll_offset: u16,
    },
    Pad { padding: Padding, child: Box<View> },
}
```

Every type derives `Debug, Clone, PartialEq` so renderer tests can
`assert_eq!` against hand-written expected Views.

Convenience constructors keep renderers terse:

```rust
View::text(s);
View::stack_v(children);
View::stack_h(children);
View::pad(view, padding);
Span::plain(s);  // unstyled span
```

`Color` mirrors ratatui's vocabulary (Reset, 16 named colors,
`Indexed(u8)`, `Rgb(u8,u8,u8)`) but is type-decoupled.

## The translator

`crates/ox-cli/src/view_render.rs` is the **only** place ratatui is
touched.

```rust
pub(crate) fn render_to_frame(
    view: &View,
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
)
```

Total over the View enum (no catch-all). Per-variant private fns.
Mapping helpers (`map_color`, `map_style`, `map_modifiers`,
`map_direction`, `map_align`, `map_sizing`) are 1:1 with ratatui's
vocabulary; adding a `Color` to ox-view forces a compile-time
match-arm here.

Snapshot tests use `ratatui::backend::TestBackend` +
`insta::assert_snapshot!` of the formatted buffer.

The translator is **dumb**. No conditional logic about *which*
variant to render based on data values; that's a renderer concern.

## Commands

```rust
pub trait Command: Send + Sync {
    fn id(&self)      -> &CommandId;
    fn display(&self) -> &CommandDisplay;
    fn scope(&self)   -> &CommandScope;
    fn run(
        &self,
        snapshot: &mut dyn Reader,
        ctx: &CommandCtx<'_>,
    ) -> Vec<Write>;
}

pub struct CommandCtx<'a> {
    pub registry:       &'a RendererRegistry,
    pub last_keystroke: Option<KeyChord>,
}

pub struct Write { pub path: Path, pub record: Record }
```

A command is a small struct + trait impl, registered by `CommandId`
in `CommandRegistry`. There are 27 day-one commands in
`crates/ox-cli/src/settings/commands/`, built via a `command!` macro
that handles the boilerplate.

**`CommandCtx` is the narrow growth bound.** Per-invocation non-data
inputs (renderer registry for ascend; just-pressed key for field
insert) go here. Data inputs (selection pointers, focus, draft text)
come from the snapshot. Ambient services (transport, config dirs)
are closed over at construction time. New `CommandCtx` fields are a
deliberate language extension.

There is **no on-the-wire effect DSL.** No `PathTemplate`, no
`PayloadSource`, no `CommandEffect`. A command is Rust code.

## Bindings

```rust
pub struct BindingEntry {
    pub screen:      Screen,
    pub cursor_path: Option<Path>,  // None = whole-screen scope
    pub mode:        Option<Mode>,
    pub key:         KeyChord,
    pub command_id:  CommandId,
}
```

Lookup specificity (most → least):

1. `cursor_path: Some + mode: Some`
2. `cursor_path: Some + mode: None`
3. `cursor_path: None + mode: Some`
4. `cursor_path: None + mode: None`

Ties broken by registration order. The registry sorts at startup;
lookup is a linear scan.

`KeyChord` is a typed struct: `{ modifiers: KeyModifierSet, code:
KeyCodeRepr }`. `KeyCodeRepr` covers `Char(char)`, `Enter`, `Esc`,
`Tab`, `BackTab`, `Backspace`, `Delete`, `Up`, `Down`, `Left`,
`Right`, `PageUp`, `PageDown`, `Home`, `End`, `Insert`, `F(u8)`.

The text-editing scope (`settings/accounts/_detail`) gets ~96 bindings
from a helper that registers one `BindingEntry` per printable ASCII
char → `field.insert`, plus one for `Backspace` →
`field.delete_back`.

## Dispatch flow

A user keypress on the settings screen flows through:

```
crossterm event
  ↓
event_loop drains keypress
  ↓
dispatch::send_key(client, key_str, Screen::Settings, flags,
                   Some(&cursor), Some(&mut snap),
                   Some(&bindings), Some(&commands),
                   Some(&renderers))
  ↓
parse_key_str(key_str) → KeyChord
  ↓
settings::dispatch::dispatch_settings_key:
  bindings.lookup(screen, cursor, mode, key) → CommandId
  commands.lookup(&command_id) → &dyn Command
  command.run(&mut snapshot, &CommandCtx { ... }) → Vec<Write>
  ↓
for write in writes: client.write(&write.path, write.record).await
  ↓
broker dispatches subscription handlers
  ↓
subscription handlers may write more (cascade-bounded, default 64)
  ↓
event loop reads ui/settings/_request_exit on next iteration
  ↓
next frame: snapshot fetch → cursor read → registry.render
            → view_render::render_to_frame
```

When the binding misses, dispatch falls through to the legacy
input-store path so global handlers (modal overlays, etc.) still get
a shot.

## Subscriptions

Subscriptions are the *only* place async work happens. They're a
protocol layered over the broker's writes.

```rust
pub trait Subscription: Send + Sync {
    fn id(&self) -> &SubscriptionId;
    fn watches(&self) -> &[PathPattern];
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write>;
}

pub struct SubCtx<'a> {
    pub snapshot: &'a mut dyn Reader,    // live, not pinned
    pub change:   &'a PathChange,
    pub spawn:    &'a dyn SpawnHandle,
    pub writer:   Arc<dyn AsyncWriter>,  // back-channel for spawned
}

pub enum PathPattern {
    Exact(Path),
    Prefix(Path),
    PrefixSuffix { prefix: Path, suffix: Path },
}
```

`PathPattern::matches` is **component-level**, never byte-level:
`Prefix(config/gate/accounts)` does not match
`config/gate/accounts_other`. The boundary lives between path
components.

Runtime contract:

1. At startup, every Subscription is registered on the broker.
2. On every successful write, the broker's `DispatchingStore`
   computes the `PathChange`, looks up matching subscriptions, calls
   each one's `handle` synchronously.
3. Writes returned from `handle` are queued and applied through the
   same dispatcher (re-triggering subscriptions, fixpoint, with
   `cascade_bound: usize` default 64).
4. Long-running async work uses `ctx.spawn(fut)` and writes back
   through `ctx.writer`. `spawn` returns a `tokio::task::AbortHandle`
   so a subscription holding a `Mutex<HashMap<String, AbortHandle>>`
   can supersede prior tasks.
5. A subscription handler that panics or returns an error is
   contained: `tracing::error!` with the subscription id; siblings
   still run; original `write()` returns Ok.
6. **Snapshot is a live reader, not pinned.** Successive
   `snapshot.read` calls within one handler may observe concurrent
   writes. Most handlers read one path; handlers that read several
   and reason about cross-path consistency must coordinate.

Day-one subscriptions live in `crates/ox-gate/src/subscriptions/`.
See `reference.md` §Subscriptions for the full table.

**Path convention**: per-instance actions live at
`<collection>/{id}/<verb>_now`. Collection-level actions live at
`<collection>/_<verb>_now` — leading `_` distinguishes sentinels from
user identifiers (which are `PathComponent::try_new`-validated and
never start with `_`).

## The snapshot

Renderers run synchronously. Reading the broker is async. Bridging
that gap is the **snapshot**: an in-memory `LocalConfig`-backed
Reader populated each frame by walking the prefixes the settings UI
cares about.

```rust
pub async fn fetch_settings_view_state(
    client: &ClientHandle,
) -> SettingsSnapshot
```

Walks 7 prefixes via `client.read_subtree`:

- `config/gate/accounts`
- `config/gate/providers`
- `config/gate/completions`
- `ui/settings`
- `ui/global`
- `settings/index/entries`
- `secret/keys`

Each `(path, value)` is inserted into the snapshot's inner store.
`SettingsSnapshot` impls `Reader` by forwarding to the inner.

The snapshot is built once per frame for rendering, and once per
keypress for dispatch (the dispatch snapshot sees post-write state
from the prior keystroke). Independent snapshots — sharing one
across the two phases would require restructuring async lifetimes
through the `terminal.draw(...)` callback. Acceptable cost; one
fewer broker round-trip on settings keys.

## Why this shape

Skip on first read. Useful when something feels weird and you want to
understand the rationale.

**A small flat View enum**, not a widget hierarchy. The renderer
constructs a `View` value; a thin translator turns it into ratatui
draw calls. Adding a "widget" requires extending the enum *and* the
translator — that cost is the point. It keeps the visual vocabulary
curated.

**Renderers pure** because they're easier to test, easier to compose
(modal-over-page recurses through the registry), and impossible to
desync from data (a stale render only happens if you held onto a
snapshot too long, which the framework prevents by rebuilding it per
frame).

**Commands pure** because the same argument applies and because
testing them is `assert_eq!` over `Vec<Write>`. A command that
spawned async work would be untestable.

**Subscriptions on the broker** rather than the renderer side because
they need to react to writes from anywhere — not just the user's
keypresses. The `t` key writes `…/test_now`; so does an
auto-validation hook on save; so could a future `ox cli` admin
command. All paths converge at the watched-pattern dispatch.

**Bindings as data** so v2 can let users edit them on disk. v1 ships
them hardcoded but the shape is forward-compat.
