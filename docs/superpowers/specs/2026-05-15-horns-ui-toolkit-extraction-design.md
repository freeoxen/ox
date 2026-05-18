# horns: extracting the settings UI system into a reusable toolkit

Status: design — 2026-05-15
Audience: implementers of the extraction; readers of the resulting framework.

## Goal

The settings UI system in `ox-cli/src/settings/` and `ox-view/` is a
path-MVU UI framework. It ships with a single user (the settings
screen) but the framework itself is general. Extract it as `horns` so
that:

1. The framework is reusable for other screens (inbox, threads,
   history) and for any future ox tool that needs a TUI.
2. The framework's shape is durable — paths and writes are the only
   interface, the View vocabulary is curated, the dispatcher is
   hierarchical over the cursor's ancestor chain.
3. The settings screen stops carrying framework code in its
   directory and becomes a clean instance of horns.

The extraction is the moment to fix the two structural compromises
that crept into the current code:

- The `ratatui` translator lives in `ox-cli/src/view_render.rs`
  rather than in a backend crate. ratatui should be one backend, not
  the framework's coupling.
- `Screen` and `Mode` enums are baked into the framework's dispatch
  and binding types. The framework should not name application
  domain enums.

## What S-tier looks like

A general-purpose path-MVU UI framework is determined by four
structural decisions, in order of leverage:

1. **Runtime contract — MVU + Subscriptions.** Renderers are pure
   `&mut dyn Reader → View`. Commands and handlers are pure
   `&mut dyn Reader → Vec<Write>`. The runtime owns the loop;
   user code never blocks, never spawns, never mutates outside
   `Vec<Write>`.
2. **State substrate — path-addressable namespace.** Every piece of
   state — cursor, focus, edit buffer, theme, view tree — lives at a
   path. Observability and inspection fall out for free.
3. **View vocabulary — closed enum + total translator.** Adding a
   widget requires extending the enum and the translator. That cost
   is the point.
4. **Effects — data, not callbacks.** Update produces `Vec<Cmd>`
   (here, `Vec<Write>`). The runtime is the only place I/O happens.

The current framework nails all four. Two additional structural
properties make horns S-tier rather than "good terminal toolkit":

- **The interface is StructFS, end to end.** horns is a *mount* on
  the broker, not a library you call. After install, all interaction
  is broker writes. No `dispatch::send_key(...)`; no
  `view_render::render_to_frame(...)`. Just paths.
- **Backend pluggable.** ratatui is one backend (`horns-ratatui`).
  Web and native backends are future drop-ins reading the same
  `View` schema off the broker.

## Crate topology

Three crates replace `ox-view` and the framework portion of
`ox-cli/src/settings/`:

```
crates/horns-core/                # framework, no ratatui, no domain enums
  src/lib.rs
  src/view.rs                     # View enum + primitive types
  src/key.rs                      # KeyChord, KeyCodeRepr, KeyModifierSet
  src/binding.rs                  # BindingEntry, BindingRegistry,
                                  # BindingScope, Phase, KeyHandler tier
  src/command.rs                  # Command, CommandRegistry, CommandCtx,
                                  # CommandId, CommandMetadata, Write
  src/render.rs                   # Renderer, RendererRegistry, RenderCtx,
                                  # AscendRule
  src/dispatch.rs                 # Dispatcher (internal; not part of public API)
  src/subscription.rs             # KeyDispatchSub, RenderSub, ThemeChangeSub
  src/install.rs                  # install(broker, opts) -> HornsHandle
  src/snapshot.rs                 # Reader-based snapshot helpers
  src/help.rs                     # generic help-view builder

crates/horns-ratatui/             # backend mount
  src/lib.rs                      # install(broker, opts) -> RatatuiHandle
  src/render.rs                   # ViewRenderSubscription + render_to_frame
  src/theme.rs                    # Theme + style helpers
  src/map.rs                      # ratatui mapping helpers

crates/horns/                     # umbrella + docs
  src/lib.rs                      # pub use horns_core::*;
                                  # #[cfg(feature = "ratatui")]
                                  # pub mod ratatui { pub use horns_ratatui::*; }
  docs/                           # moved from docs/ui_framework{,/*}
    ui_framework.md
    architecture.md
    howto.md
    reference.md
```

Dependencies:

- `horns-core` depends on `structfs-core-store` and `ox-broker`.
  ox-broker carries the substrate (`Subscription`,
  `SubscriptionRegistry`, `Write`, `PathChange`). The dependency is
  intentional: horns is a mount on the broker, so it must speak the
  broker's vocabulary. Future spinout could extract
  `structfs-broker-traits` as a smaller surface, but that's out of
  scope.
- `horns-ratatui` depends on `horns-core`, `ratatui`, `unicode-width`.
- `horns` depends on `horns-core`; the `ratatui` feature pulls
  `horns-ratatui`.

`ox-view` is deleted (folded into `horns-core/src/view.rs`).
`ox-types` splits: framework types (`KeyChord`, `BindingEntry`,
`CommandId`, `BindingScope`, `Phase`, `Write`) move to `horns-core`;
domain types (`SettingsIndexEntry`, `AccountField`, `ModelKey`,
`GlobalBanner`, `ValidationDiagnostics`, `Screen`, `Mode`, etc.) stay
in `ox-types`.

## The interface is StructFS

After `install`, the host never calls horns. Every interaction is a
broker write or a broker read.

### `install`

```rust
pub struct InstallOptions {
    // Path config — where the host wants horns to read and write.
    pub cursor_path:        Path,   // where this horns instance reads focus from
    pub input_path:         Path,   // <input_path>/key, <input_path>/area, ...
    pub render_tick_path:   Path,   // increment to request a re-render
    pub render_output_path: Path,   // horns writes the resulting View here
    pub bindings_prefix:    Path,   // <bindings_prefix>/<binding-id>: BindingEntry
    pub commands_prefix:    Path,   // <commands_prefix>/<command-id>: CommandMetadata
    pub renderers_prefix:   Path,   // <renderers_prefix>/<cursor-path-id>: RendererMetadata
    pub handlers_prefix:    Path,   // <handlers_prefix>/<handler-id>: HandlerMetadata
    pub theme_path:         Path,   // current theme

    // Code wiring — closures the host registers atomically with install.
    pub commands:  HashMap<CommandId,  Box<dyn Command>>,
    pub renderers: HashMap<Path,       Box<dyn Renderer>>,
    pub handlers:  HashMap<HandlerId,  Arc<dyn KeyHandler>>,

    // Initial data — written to the broker as part of install.
    pub bindings: Vec<(BindingId, BindingEntry)>,
    pub theme:    Theme,
}

pub fn install(broker: &mut Broker, opts: InstallOptions) -> HornsHandle;

pub struct HornsHandle { /* opaque; only operation is `unmount` */ }
```

`install` atomically:

1. Writes the metadata for every binding, command, renderer, and
   handler to the broker under their configured prefixes.
2. Writes the initial theme to `theme_path`.
3. Stores the closures in `HornsHandle`'s in-process side-tables,
   indexed by their IDs.
4. Registers three subscriptions on the broker: `KeyDispatchSub`,
   `RenderSub`, `ThemeChangeSub`.

After `install` returns, the host's only horns interface is broker
writes.

### What lives at paths

```
<bindings_prefix>/<binding-id>          BindingEntry { scope, key, phase, command_id }
<commands_prefix>/<command-id>          CommandMetadata { display, scope }
<renderers_prefix>/<cursor-path-id>     RendererMetadata { ascend_rule }
<handlers_prefix>/<handler-id>          HandlerMetadata { scope, phase, class }
<theme_path>                            Theme
<input_path>/key                        KeyChord
<input_path>/area                       Area { w, h }
<render_tick_path>                      u64
<render_output_path>                    View
<cursor_path>                           Path
```

Code (closures) does not live at paths; closures can't be
serialized. The data half of every registration lives at a path; the
code half lives in `HornsHandle`'s side-tables keyed by the ID
that's also at the path. The link between data and code is the ID.

This is how Compose's `CompositionLocal` works and how a Lisp's
symbol table works: names are first-class data; bindings of names to
values can be code; the resolution layer keeps them in sync.

### The three internal subscriptions

Registered on the broker at install time. The host doesn't see them.

**`KeyDispatchSubscription`** — watches `<input_path>/key`:

1. Reads cursor from `<cursor_path>`.
2. Computes scope path = `cursor.ancestors()`.
3. Per phase (Capture outer→inner, Target leaf only, Bubble
   inner→outer):
   - Looks up discrete bindings at the scope from `<bindings_prefix>`,
     ranked by specificity. First match wins.
   - If no discrete match: looks up handlers at the scope from
     `<handlers_prefix>`, in registration order. Each handler's
     closure runs with the snapshot; first returning `Some(writes)`
     wins.
4. Resolves the matched command/handler closure from the side-table.
5. Runs it with `&mut dyn Reader`.
6. Returns the resulting `Vec<Write>`. Broker cascades them through
   `DispatchingStore`.
7. After the cascade settles, writes `<render_tick_path> += 1` to
   trigger a re-render.

**`RenderSubscription`** — watches `<render_tick_path>` and
`<cursor_path>`:

1. Reads cursor.
2. Looks up the renderer for that cursor from `<renderers_prefix>` +
   side-table.
3. Reads theme from `<theme_path>`.
4. Builds a `RenderCtx` and runs the renderer.
5. Writes the resulting `View` to `<render_output_path>`.

**`ThemeChangeSubscription`** — watches `<theme_path>`:

1. On any write, increments `<render_tick_path>` to trigger a
   re-render with the new palette.

### Event loop becomes thin

```rust
loop {
    match crossterm::event::read() {
        Event::Key(k) => {
            let chord = parse_chord(k);
            client.write_typed(&input_key_path, &chord).await?;
            // KeyDispatchSub fires; its cascade ends with a render-tick bump.
        }
        Event::Resize(w, h) => {
            client.write_typed(&input_area_path, &Area { w, h }).await?;
            client.write_typed(&render_tick_path, &next_tick()).await?;
        }
        _ => {}
    }
}
```

The event loop imports no horns types except the path constants the
host configured at install time. The paths *are* the interface.

### The backend is also a mount

`horns-ratatui::install` registers a `ViewRenderSubscription`
watching `<render_output_path>`:

1. On write, reads the `View`.
2. Reads the current theme from `<theme_path>`.
3. Calls `render_to_frame(view, frame, area, theme)` on the
   `Terminal` the host passed in.

```rust
pub struct RatatuiOptions {
    pub view_input_path: Path,
    pub theme_path:      Path,
    pub terminal:        Arc<Mutex<Terminal<CrosstermBackend<Stdout>>>>,
}

pub fn install(broker: &mut Broker, opts: RatatuiOptions) -> RatatuiHandle;
```

horns and horns-ratatui are coupled by *the View schema written to a
broker path*, not by Rust types. Replace horns-ratatui with
horns-web (DOM patches) or horns-iced (native) without touching
horns-core.

## Dispatch: capture, target, bubble over the cursor's ancestors

`compute_scope_path` is the cursor's ancestor chain. For cursor
`<page>/<widget>/<leaf>` the scope path is
`[<page>, <page>/<widget>, <page>/<widget>/<leaf>]`. Each entry is
`BindingScope::Exact(...)`.

The dispatcher walks the scope path in three phases:

1. **Capture** (outer → inner): lifecycle keys an outer scope claims
   regardless of what's nested below. Esc to cancel a form, Tab to
   advance focus.
2. **Target** (leaf only): the focused leaf claims its semantic
   keys. A text leaf claims printable ASCII; a selector leaf claims
   `h`/`l`.
3. **Bubble** (inner → outer): keys the leaf didn't consume bubble
   up. Enter on a form is bubble; a future multiline text leaf
   could claim Enter at Target for newline insertion and the form's
   commit handler still works.

At each scope/phase, two lookup tiers run in order:

1. **Discrete bindings** (introspectable): `BindingEntry { scope, key,
   phase, command_id }`. Specificity-ranked (`Exact > Prefix(deeper) >
   Prefix(shallower) > Anywhere`); first match wins.
2. **Handlers** (opaque): `HandlerEntry { scope, phase, handler }`.
   Registration order; first returning `Some(writes)` wins.

`BindingScope::Anywhere` is the lowest-specificity tier; it is not
pushed onto the scope path but rides into per-scope lookup through
the registry's specificity ordering. An Anywhere+Bubble binding is
the final fallback at every Bubble query; an Anywhere+Capture
binding fires at the outermost scope before any inner scope sees the
key.

`Screen` and `Mode` are gone from the framework. There is no
`screen: Screen` parameter, no `mode: Option<Mode>` lookup. Apps with
multiple screens or modes either:

- Install multiple horns instances at disjoint path namespaces (one
  per logical screen). Each instance reads its own cursor and has
  its own registry contents under its own prefixes.
- Encode the screen/mode in cursor path segments. The cursor at
  `inbox/threads/<id>` is *on the inbox screen, on the threads page,
  focused on thread X*. The scope path's outer entries carry the
  screen context naturally.

## Opaque event consumers: discrete vs handler

The text-field-style "consumes any printable ASCII" pattern doesn't
fit discrete bindings — 96 BindingEntries for one moral statement
("this field claims typeable keys") is a smell. The handler tier
fixes it.

A handler is a closure registered against scope + phase. The
dispatcher's per-phase walk asks discrete bindings first
(introspectable, ranked by specificity); if no match, asks handlers
in registration order. Each handler inspects the key and returns
`Some(Vec<Write>)` to claim or `None` to pass.

```rust
pub trait KeyHandler: Send + Sync {
    fn handle(
        &self,
        snapshot: &mut dyn Reader,
        key:       &KeyChord,
        ctx:       &CommandCtx<'_>,
    ) -> Option<Vec<Write>>;
}
```

`Some(vec![])` is a legitimate claim (consumed, no state change).
`None` means dispatch continues to the next tier or scope.

The settings `_edit` scope migrates from 96 discrete bindings to one
`TextInputHandler` + 4 lifecycle bindings (Backspace, Enter, Esc,
Tab) in the same PR. That's the canonical demonstration of
encapsulation; without it the spec promises opacity but the first
user doesn't show it.

### Discrete vs handler — when to choose

- **Discrete** when the key has a *named* meaning the parent's help
  screen should show, the user's config-on-disk should be able to
  override, or an accessibility audit should be able to enumerate.
  Lifecycle keys (Esc, Tab, Enter, Backspace, arrows), command keys
  (`a`, `d`, `?`, `Ctrl+S`).
- **Handler** when the widget claims a *range* of inputs that
  doesn't deserve named entries. Printable ASCII for text fields,
  arrow-key navigation in custom grids, anything where enumerating
  every chord would be both verbose and uninformative.

Don't unify the two into a single
`Matcher = Exact(KeyChord) | Predicate(Fn)` entry — the introspection
contract is exactly the difference between "named, audible" and
"opaque, consumed."

## Recursive composability

horns is recursively composable because the design's foundations are:
the cursor is one path; the scope path is `cursor.ancestors()`;
bindings/handlers are keyed by exact paths. None of these scale with
nesting depth.

**Single-screen nesting (compound widget inside compound widget):**
A widget at `settings/_compose_form` containing a widget at
`settings/_compose_form/_picker` containing a leaf at
`settings/_compose_form/_picker/field` produces a scope path of
length four. Capture walks all four outer→inner; Target hits the
leaf; Bubble walks all four inner→outer. Each level gets its
phase. No dispatcher changes for arbitrary depth.

**Reusable widget install pattern:** A reusable widget (date picker,
file picker, command palette) exports an `install` function:

```rust
// hypothetical horns-datepicker
pub fn install(
    namespace:  Path,                       // cursor namespace the widget owns
    ui_prefix:  Path,                       // working state subtree
    options:    DateOpts,
    bundle:     &mut HornsInstallBundle,    // mutable accumulator for horns::install
);
```

The host calls `install` per sub-widget at construction. The widget
adds its discrete bindings, handlers, commands, renderers to the
bundle under paths it owns. The bundle is then passed to
`horns::install`. Multi-instance support: install twice at different
namespaces, get two independent instances.

The widget's bindings *live in the parent's binding subtree* but are
*scoped to paths the parent doesn't otherwise use*. That distinction
is the encapsulation contract:

- Not hidden — parent can enumerate every binding for help/audit/
  override.
- Isolated — sibling instances don't conflict because the cursor is
  a single path and only one can be on its ancestry at a time.

**Sub-mounted horns instances:** the path-based interface means a
host can `horns::install` multiple times at disjoint broker
namespaces. Each instance is independent — its own cursor, its own
input path, its own render output. Use case: an embedded thread-
detail preview inside a different screen, driven by horns with its
own keybindings.

**Phase semantics let the parent fight for keys:** if a parent
registers `Esc` at Capture, the parent's Esc wins over any nested
widget's Esc (Capture is outer→inner). If the parent registers `Esc`
at Bubble, the nested widget's Esc wins (Bubble walks inner→outer
through Capture and Target first). Each level chooses Capture
(absolute claim) or Bubble (conditional claim).

## The View enum and the ratatui translator

```rust
pub enum View {
    Empty,
    Text { spans: Vec<Span>, align: Align },
    Stack { dir: Direction, children: Vec<(View, Sizing)> },
    Frame { title: Option<String>, title_right: Option<String>, content: Box<View> },
    List  { items: Vec<ListItem>, selected: Option<usize> },
    Form  { rows: Vec<FormRow>, focused: Option<usize> },
    Modal { background: Box<View>, foreground: Box<View>, dim: bool },
    Banner { kind: BannerKind, content: String },
    StatusBlock { title: String, lines: Vec<StyledLine>, scroll_offset: u16 },
    Pad { padding: Padding, child: Box<View> },
}
```

Lives in `horns-core/src/view.rs`. No serde, no ratatui. Derives
`Debug, Clone, PartialEq` so renderer tests use `assert_eq!`.

The translator (`horns-ratatui/src/render.rs`) is total over the
enum. Per-variant private functions. Mapping helpers (`map_color`,
`map_style`, `map_modifiers`, `map_direction`, `map_align`,
`map_sizing`) are 1:1 with ratatui's vocabulary; adding a `Color`
variant forces a compile-time match-arm.

Snapshot tests use `ratatui::backend::TestBackend` +
`insta::assert_snapshot!` of the formatted buffer.

The translator is dumb: no conditional logic about *which* variant
to render based on data values. That's a renderer concern.

A subsequent revision must teach the View enum to serialize so it
can ride the broker path (`<render_output_path>: View`). Adding
`serde::{Serialize, Deserialize}` is mechanical given the curated
shape; the enum has no closures or non-data fields.

## ox-cli wiring after extraction

Settings becomes a clean horns instance. Its remaining shape in
`ox-cli/src/settings/`:

```
ox-cli/src/settings/
  mod.rs               # exports settings::install(broker, ...)
  bindings.rs          # builds Vec<(BindingId, BindingEntry)> for InstallOptions
  bootstrap.rs         # populate_index_entries, maybe_first_run_cursor, ...
  visible_rows.rs      # settings-specific projection
  snapshot.rs          # SettingsSnapshot prefix list (uses horns::Snapshot helpers)
  commands/            # settings-specific Command implementations
  renderers/           # settings-specific Renderer implementations
```

`ox-cli/src/settings/mod.rs` exposes one function:

```rust
pub fn install(broker: &mut Broker) -> SettingsHandle {
    let mut bundle = HornsInstallBundle::new();
    bindings::register_all(&mut bundle);
    commands::register_all(&mut bundle);
    renderers::register_all(&mut bundle);

    let horns_handle = horns::install(broker, bundle.finish(InstallOptions {
        cursor_path:        oxpath!("ui", "settings", "focused"),
        input_path:         oxpath!("ui", "_horns", "settings", "input"),
        render_tick_path:   oxpath!("ui", "_horns", "settings", "render", "tick"),
        render_output_path: oxpath!("ui", "_horns", "settings", "render", "output"),
        bindings_prefix:    oxpath!("horns", "settings", "bindings"),
        commands_prefix:    oxpath!("horns", "settings", "commands"),
        renderers_prefix:   oxpath!("horns", "settings", "renderers"),
        handlers_prefix:    oxpath!("horns", "settings", "handlers"),
        theme_path:         oxpath!("ui", "theme"),
        theme:              default_theme(),
    }));

    let ratatui_handle = horns::ratatui::install(broker, /* ... */);

    SettingsHandle { horns_handle, ratatui_handle }
}
```

The settings screen is now a horns instance, named by its broker
prefix. The inbox screen (when migrated) is another horns instance
at `horns/inbox/...`. The framework is the same; the screens differ
only in their bindings, commands, renderers, and configured paths.

## ox-types split

| Moves to `horns-core` | Stays in `ox-types` |
|---|---|
| `KeyChord` | `Screen` (app discriminator) |
| `KeyCodeRepr` | `Mode` (app discriminator) |
| `KeyModifierSet` | `SettingsIndexEntry`, `BadgeSource` |
| `BindingEntry` | `AccountField`, `ModelField`, `ModelKey` |
| `BindingScope` | `ValidationDiagnostics`, `GlobalBanner` |
| `Phase` | `CompletionRole`, `ModelInfo`, `ModelInfoSource` |
| `CommandId`, `CommandDisplay`, `CommandScope` | `path_serde` (stays; broker dep) |
| `Write` | |

`PathPattern`, `PathChange`, `SubscriptionId` remain in their
current crates (split across `ox-types` and `ox-broker`); horns-core
imports them through ox-broker.

ox-types' `Cargo.toml` adds `horns-core` as a dep where it still
references these types after the move (e.g., binding fixtures in
tests). Most settings domain types don't reference framework types,
so the new dep is light.

## Doc migration

`docs/ui_framework.md` + `docs/ui_framework/{architecture,howto,
reference}.md` move to `crates/horns/docs/`. The move happens in one
commit so git history follows; sanitization happens in the next
commit.

Sanitization touches:

- Replace `Screen::Settings` everywhere with `<your screen>` or
  drop it.
- Replace `ui/settings/focused` with `<focus cursor path>` and
  factor the path-config story into a section that says "the host
  configures these paths at install time."
- Replace settings-specific examples (`settings/accounts/<name>`,
  `ui/settings/new_account/buffer`) with generic examples
  (`<page>/<row>`, `<page>/_<widget>/buffer`).
- Keep one section in `howto.md` titled *"The settings screen as a
  worked example"* that walks through the concrete install. The
  example demonstrates without dominating.
- Add new sections to `architecture.md`: *"Mount, not library"*,
  *"Recursive composability"*, *"Opaque handlers vs introspectable
  bindings"*.
- Add a section to `howto.md`: *"Shipping a reusable widget"* with
  the `install(namespace, prefix, opts, &mut bundle)` pattern.

Update `reference.md`'s type signatures and paths to match the
post-extraction API.

## Migration plan

One PR, 13 numbered commits, each compiles independently:

1. Create empty `horns-core`, `horns-ratatui`, `horns` crates;
   wire into the workspace; add empty `lib.rs`.
2. Move `ox-view/src/lib.rs` → `horns-core/src/view.rs`. Delete
   `ox-view` crate. Update consumers (ox-cli) to import from
   `horns-core::view::*`.
3. Move `ox-types/src/{key_chord,command_binding}.rs` →
   `horns-core/src/{key,binding,command}.rs`. ox-types gains
   `horns-core` dep where still referenced.
4. Move `ox-cli/src/settings/{registry,command_registry,
   binding_registry}.rs` → `horns-core/src/{render,command,
   binding}.rs` (merge into existing). Strip `Screen` and `Mode`
   parameters from the public API. Update ox-cli call sites to
   match.
5. Move `ox-cli/src/settings/dispatch.rs` → `horns-core/src/
   dispatch.rs`. Rename `dispatch_settings_key` →
   `Dispatcher::dispatch`. Make `Dispatcher` internal (not part of
   public API).
6. Move `ox-cli/src/view_render.rs` + `ox-cli/src/theme.rs` →
   `horns-ratatui/src/{render,theme,map}.rs`. Add serde derives to
   `View` and supporting types in `horns-core/src/view.rs` so the
   view tree can ride a broker path.
7. Add `KeyHandler` trait + handler tier to
   `horns-core/src/binding.rs`. Tests cover handler/discrete
   interaction at each phase.
8. Add subscription scaffolding to
   `horns-core/src/subscription.rs` (KeyDispatchSub, RenderSub,
   ThemeChangeSub). Add `install(broker, opts)` to
   `horns-core/src/install.rs`. Add `horns-ratatui::install(...)`
   to `horns-ratatui/src/lib.rs`. Tests cover the install
   lifecycle.
9. Migrate `ox-cli/src/settings/_edit` from 96 discrete bindings
   to a `TextInputHandler` + 4 lifecycle bindings.
10. Rewire `ox-cli/src/event_loop.rs` from `dispatch::send_key`
    calls to broker writes. Delete `ox-cli/src/dispatch.rs`.
    Replace `view_render::render_to_frame` callsite with
    `horns::ratatui::install`.
11. Move docs `docs/ui_framework.md` + `docs/ui_framework/` →
    `crates/horns/docs/`.
12. Sanitize docs in place. Add new architecture sections.
13. Add a benchmark comparing today's settings dispatch vs the new
    path-writing flow.

Sizing: ~5k lines moved + ~1k lines new + ~500 lines changed in
ox-cli. Each commit is independently bisectable.

## Testing

The broker-mount shape improves testability:

- **horns-core unit tests:** drive subscriptions via fixture
  Readers; assert on returned `Vec<Write>`. Same shape as today's
  settings tests, framework-agnostic.
- **horns-ratatui snapshot tests:** write a `View` to the fixture
  view-input path; run the `ViewRenderSubscription`; assert against
  `insta` snapshot of the `TestBackend` buffer. Same shape as
  today's `view_render` snapshot tests.
- **End-to-end settings tests:** install horns + horns-ratatui on a
  test broker; write key chords to the input path; read the
  resulting `View` from the render-output path. Tests now exercise
  the full mount lifecycle without any direct horns function
  calls — they look exactly like how a real client uses horns.

Existing settings tests in `ox-cli/src/settings/` continue to work;
some are rewired from "call `dispatch_settings_key` directly" to
"write to the input path, observe the cascade." That rewiring is
straightforward and the per-commit testing structure exercises it.

## Risks and trade-offs

**Sizing.** ~5k lines moved + ~1k new + ~500 changed. Genuinely big
PR. Mitigation: 13 numbered commits above, each compiles
independently; reviewable per-commit.

**Subscription cascade ordering.** KeyDispatch → cascade →
RenderTick depends on `DispatchingStore`'s cascade-bounded fixpoint
completing before render fires. Today's cascade bound is 64;
sufficient for any realistic command. Mitigation: a single
integration test exercising a multi-write cascade through dispatch →
render proves the path end-to-end.

**Broker coupling.** `horns-core` depends on `ox-broker` for
`Subscription` + `SubscriptionRegistry`. The dep is intentional
(broker is the substrate) but means horns can't be used standalone
without ox-broker. Acceptable since the project ships them together;
a future spinout could extract `structfs-broker-traits` as a smaller
surface.

**Performance.** Three subscriptions per keystroke vs today's
function-call chain. Broker dispatch overhead is on the order of
microseconds; should be invisible. Mitigation: a benchmark
(commit 13) comparing today's settings dispatch vs the new
path-writing flow lands with the PR.

**`_edit` migration parallel to infrastructure migration.** Risk of
bug-mixing. Mitigation: `_edit` → handler migration is commit 9,
after horns infrastructure is in place at commit 8. Each commit is
bisectable.

**Trade-offs the spec deliberately makes:**

- *Bindings as data, serialized to the broker.* Alternative: keep
  bindings in-memory, expose via a computed read-only path. Chosen:
  full serialization because it enables disk-overrides cleanly. Cost:
  binding registration is broker-round-trip (mitigated by atomic
  install).
- *Renderer keyed by `cursor-path-as-id`.* The metadata path uses
  the cursor path stringified as an id. Stringification is
  reversible (`Path::parse` round-trips). Workable.
- *No type parameters for Screen/Mode.* Apps that want
  multi-screen install one horns instance per screen at disjoint
  prefixes, or encode the screen in cursor segments. Generic
  parameters were rejected as type-ceremony for the user.
- *Discrete and handler tiers separate, not unified.* Could fold
  into one `Matcher = Exact(KeyChord) | Predicate(Fn)` entry but
  the introspection contract is exactly the difference between
  "named, audible" and "opaque, consumed."

## Out of scope

- Live keybinding editing UX (the data is at paths, but no UI
  surfacing the binding subtree as editable).
- Hot reload of commands or renderers (the side-table is mutable
  via `HornsHandle` methods, but no driver to swap closures at
  runtime).
- Non-ratatui backends (the architecture supports them; no second
  backend ships in this extraction).
- Migrating inbox/threads/history screens to horns (one screen at a
  time; future work).
- Multi-process broker transport (out of scope; the architecture
  supports it).
- `structfs-broker-traits` spinout from ox-broker (out of scope;
  the horns-core → ox-broker dep is accepted).

## What success looks like

After this PR:

1. `ox-view`, `ox-cli/src/dispatch.rs`, and `ox-cli/src/view_render.rs`
   no longer exist; their content lives in horns crates.
2. `ox-cli/src/settings/` contains only settings-specific code:
   bindings table, commands, renderers, bootstrap, visible_rows,
   snapshot. The framework directory is empty.
3. `ox-cli/src/event_loop.rs` imports no horns types except path
   constants the host configured at install time.
4. The settings screen behaves identically to before from the
   user's perspective.
5. The text-input scope is one handler + four lifecycle bindings,
   not 96 BindingEntries.
6. The docs at `crates/horns/docs/` describe the framework
   generically; the settings screen appears as a worked example.
7. The settings test suite passes. A new integration test exercises
   the full mount lifecycle via broker writes.

After this PR, adding a second horns screen (inbox, threads, ...)
is:

1. Write its `BindingId → BindingEntry` table.
2. Write its `Command` implementations.
3. Write its `Renderer` implementations.
4. Build an `InstallOptions` with disjoint broker prefixes.
5. Call `horns::install(broker, opts)` and `horns::ratatui::install`.

No new framework code. No `Screen::Inbox` enum value. No dispatcher
change. Just install at a different namespace.
