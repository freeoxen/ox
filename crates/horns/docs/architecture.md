# UI framework — architecture

How the pieces fit together. Read this once to get the mental model;
after that, work from `howto.md`.

## Three trees

The framework runs over three logically distinct trees, all keyed by
`Path`:

- **Data tree** — broker mounts under `config/`, `secret/`, and the
  data side of `settings/index/entries`. Persistent or
  ephemeral *facts about the world*: AccountConfig, ApiKey,
  ProviderConfig, model catalogs, validation status. A write to a
  data-tree path is the act of changing the world: writing
  `config/gate/accounts/<name>` creates an account, writing `Null`
  deletes it.
- **Display tree** — broker mounts under `ui/`. Where the user is
  right now and what UI state they're in: cursor, focus, selection
  pointers, edit buffer, pending-delete target, composing-name
  buffer. The user's active widget is encoded by where
  `ui/settings/focused` points — a cursor under
  `settings/_compose_form` means the compose widget is engaged. Per-
  cursor working state (the typed buffer, the saved pre-open cursor,
  staged drafts) lives at named UI-state paths like
  `ui/settings/new_account/buffer`. The display tree is data too —
  just data about the UI rather than the world.
- **View tree** — constructed each frame by renderers. A curated
  `View` enum. In-memory only; no serde, no ratatui.

The data tree and the display tree share one substrate (the
namespace) but partition cleanly by prefix. Renderers read both;
commands write both; subscriptions watch only data-tree paths (UI
state is never the trigger for async work).

## The cursor

`ui/settings/cursor: Path` holds the path of the page the user is
currently viewing. Default: `oxpath!("settings", "index")`.

`ui/settings/focused: Path` is the **universal focus authority** —
the single source of truth for what is currently focused. It points
at a row, a compound widget root, or a compound widget sub-element.
Its ancestor chain is the scope path the dispatcher walks; the
cursor at `ui/settings/cursor` is its page-level outer scope.
See *Cursor as universal focus* below.

The cursor is a *page* pointer, not a *mode* pointer. Pages are
distinct screens you navigate to (`settings/index`,
`settings/accounts`, `settings/models`). Compound-widget modes
(composing, confirming, editing inline, manual model entry) are
encoded in `focused`'s path segments: `settings/_compose_form/name`
puts the cursor inside the compose widget on its `name` field. UI
sub-states tied to a specific cursor position — like an inline edit
buffer — still live as values at named UI-state paths. See
*Modeling state* below.

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

`_request_exit` is one of a small handful of legitimate sentinel
paths: it carries a cross-component signal (the event loop reads it
to know when to switch screens) that genuinely has no other home. It
is not a mode — there is no "exiting" state the user occupies. The
write is the signal.

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

**Composition is value-shaped, not call-shaped.** A renderer reads
both data-tree and display-tree state and emits a View that reflects
both. The accordion's accounts section, for example, reads the list
of real accounts from the data tree AND the inline-create buffer
from the display tree, then composes them into a single rendered
section:

```rust
fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
    let mut rows = self.real_account_rows(ctx);
    if let Some(buffer) = read_typed::<String>(
        ctx.data,
        &oxpath!("ui", "settings", "new_account", "buffer"),
    ) {
        // Composing a new connection — render an inline name prompt
        // at the top of the section, decorated with the live buffer.
        rows.insert(0, inline_name_prompt(&buffer));
    } else {
        // Idle — render the static "+ New connection" affordance.
        rows.insert(0, static_create_affordance());
    }
    View::List { title: Some("Connections".into()), items: rows, selected: …  }
}
```

The `+ New connection` affordance is *always* visible in some form,
but it's a renderer-side decoration reading UI-mode state — never a
synthetic row in the visible-rows projection, never a navigable
cursor scope. Renderers that compose multi-page state (e.g. an index
page that summarizes selection counts from sub-areas) can recurse
into the registry; that's the same value-shaped composition.

## Modeling state: modes vs places

The hardest design question in this framework is "where does this
piece of state belong?" The answer follows from three commitments
the framework makes — listed in `ui_framework.md` and elaborated
here.

### Mode is state, not place

Two distinct kinds of "mode" appear in the UI; they belong in
different namespaces.

**Compound-widget modes** (composing a new account, confirming a
delete, editing a field inline, manual-model entry) are encoded by
where the cursor (`ui/settings/focused`) points. The cursor at
`settings/_compose_form/name` *is* the "composing a new account, on
the name field" state. There is no separate `new_account/active`
flag, no `manual_model/stage` discriminator, no `edit_mode` bool —
the cursor's path segments carry that information directly. See the
*Cursor as universal focus* section for the full pattern.

**UI sub-states tied to a specific cursor position** (the typed
text buffer for the engaged edit, the saved pre-open cursor, the
target row of a pending delete, staged draft fields for a multi-
stage form) are values at named UI-state paths. They are working
state read by the renderer and the dispatched commands; they are
*not* discriminators of which widget is engaged.

When the user opens settings, the cursor is at `settings/index`.
When they descend into accounts, the cursor moves to
`settings/accounts`. When they press `a` to add a connection, the
cursor moves to `settings/_compose_form/name` — they're still
"on the accounts page" semantically, but the focus authority is now
inside the compose widget. Pressing Esc cancels the widget by
reading the saved cursor at `ui/settings/new_account/cursor_saved`
and writing it back to `focused`, then cascade-clearing the
`new_account` subtree.

The renderer reads `ui/settings/focused` together with
`ui/settings/new_account/buffer` and decorates the accounts section
with the inline name prompt when the cursor is under
`settings/_compose_form`. The dispatcher reads the cursor's
ancestor chain and routes keys through the `_compose_form` and
`_compose_form/name` scopes.

Mapping of compound widgets to cursor paths and the per-cursor
working state they read:

| Mode | Cursor path | Per-cursor working state |
|---|---|---|
| Composing new account | `settings/_compose_form/{name,protocol,key,...}` | `ui/settings/new_account/{buffer, key, protocol, errors, cursor_saved, ...}` |
| Confirming a delete | `settings/_confirm_delete` | `ui/settings/pending_delete/{target_account, cursor_saved}` |
| Editing a field inline | `settings/_edit` | `ui/settings/edit/{target_path, buffer, cursor_saved}` |
| Manual model entry | `settings/_manual_model/{id,ctx,out}` | `ui/settings/manual_model/{buffer, account, staged_id, staged_ctx, ...}` |

When you reach to encode "the user is doing X," ask: would they
*navigate* to that scope, or *enter* a state? If they enter a state,
the cursor moves into that widget's synthetic namespace
(`settings/_<widget>`) and the renderer + dispatcher key off that.
The Esc key on a widget cancel-restores the saved cursor; on a page
it ascends to the parent page. They're different verbs in the
user's head, and they remain different verbs in the framework —
distinguished now by *which scope's binding fires* rather than by
checking a discriminator value.

Retired discriminator paths (do not reintroduce):

- `ui/settings/new_account/active: bool` — replaced by the cursor
  being under `settings/_compose_form`.
- `ui/settings/manual_model/stage: ManualModelStage` — replaced by
  the cursor's leaf segment under `settings/_manual_model`.
- `ui/settings/pending_delete: Option<AccountName>` (as a value
  flag) — replaced by the cursor at `settings/_confirm_delete`. The
  target account moved to a child path
  (`ui/settings/pending_delete/target_account`) where it is
  working state, not a flag.
- `ui/settings/edit_mode: bool` + `edit_field_path: Option<Path>` —
  replaced by the cursor at `settings/_edit`; the edited field path
  is now `ui/settings/edit/target_path`.

### Display tree names only real things

Every path in the display tree (`settings/…`, `ui/…`) names either:
- a real thing in the data tree (e.g. `settings/accounts/<name>` is
  the display identifier for the real account at
  `config/gate/accounts/<name>`), OR
- a UI-state value with semantic meaning (e.g.
  `ui/settings/edit_buffer` carries the user's live typed input).

There is no third category. There are no synthetic identifier paths
(`settings/accounts/_new`, `settings/accounts/_delete`,
`settings/models/<account>/_empty`). Synthetic UI affordances —
"+ New connection", "no models — refresh", "+ add model manually" —
are renderer-side decorations reading UI-mode state, not rows in the
visible-rows projection.

This is what makes path-equality dispatch safe. When `tree.activate`
or `edit.commit` does `rows.iter().find(|r| r.path == field_path)`,
every row in the projection names a real thing. There's no
synthetic competing for the same namespace; no `_`-prefix reservation
rule to maintain; no risk that a hand-edited TOML config could plant
a real account at a path that the framework was using for a UI
affordance.

The visible-rows projection is the cleanest expression of this:

```rust
pub fn enumerate(data: &mut dyn Reader) -> Vec<VisibleRow> {
    // Only data-tree-derived rows. The renderer composes synthetic
    // affordances on top by reading UI-mode state separately.
    self.real_account_rows(data)
        .chain(self.real_model_rows(data))
        .collect()
}
```

### A write IS the action

Subscriptions are reactive observers, not RPC handlers. The CLI's
command handlers perform data writes directly:

```rust
// commands/account_model.rs — the CLI commits an account creation.
fn commit_new_account(snap: &mut dyn Reader) -> Vec<Write> {
    let name = read_buffer(snap);
    vec![
        // The actual create — a write to the canonical data path.
        write_typed(&account_path(&name), &AccountConfig::default()),
        // UI cascade — focus the new row, expand it, clear the
        // compose widget's working state subtree.
        write_path(&focused_path(), &row_path(&name)),
        update_expanded_to_include(&row_path(&name)),
        clear_compose_subtree(),
    ]
}
```

A subscription watching `Prefix(config/gate/accounts)` may then react
to the new entry by spawning a catalog fetch. That's
async/cross-cutting work — exactly what subscriptions are for. The
subscription does *not* "create the account in response to a
sentinel write"; the CLI created the account directly, and the
subscription is doing follow-up work that requires HTTP.

The decision rule:

- Can the work be done with a single synchronous write? → CLI writes
  the data path directly. No subscription.
- Does the work require async (HTTP, file IO) or touch many paths
  with cross-cutting consistency? → CLI writes a data path; a
  subscription watches that path and does the side effects.
- Does the trigger represent "the user requested action X that can
  *only* happen asynchronously" (connectivity test, catalog
  refresh)? → A `…/test_now` or `…/refresh_now` Null-write trigger
  is legitimate — there's no other shape for "please do this async
  thing."

The anti-pattern is: CLI writes a sentinel; subscription reads the
sentinel; subscription does what the CLI could have done
synchronously. That's RPC indirection through the substrate, and
it's banned.

### Cursor as universal focus

`ui/settings/focused` is the single source of truth for what is
currently focused. The cursor's path encodes the full focus state
— row, compound widget, or compound widget sub-element.

- Cursor at a row path (e.g., `settings/accounts/alpha`) → that row
  is focused; no compound widget is active.
- Cursor at a compound widget root (e.g.,
  `settings/_confirm_delete`) → the widget is focused as a whole;
  no sub-element selected.
- Cursor at a compound widget sub-element (e.g.,
  `settings/_compose_form/name`) → that sub-element is focused; the
  widget is active by virtue of being on the cursor's ancestor
  chain.

`compute_scope_path` is the cursor's ancestor chain:

```rust
fn compute_scope_path(snap: &mut dyn Reader) -> Vec<BindingScope> {
    let Some(cursor) = read_cursor(snap) else { return Vec::new(); };
    path_ancestors(&cursor).into_iter().map(BindingScope::Exact).collect()
}
```

Bindings registered at any scope on this chain are reachable. The
dispatcher walks the chain in three phases: Capture outer→inner,
Target on the leaf, Bubble inner→outer.

Mode discriminator paths (`new_account/active`,
`pending_delete: Option<_>` as a value flag, `manual_model/stage`,
`edit_mode` + `edit_field_path`) are retired. Active mode is
implicit in cursor's path segments.

Each compound widget follows the same pattern:

- **Open**: save current cursor at `ui/settings/<widget>/cursor_saved`;
  write the new cursor to the widget's path (its root, or the first
  sub-element); initialize the working state subtree.
- **Sub-element navigation**: write the cursor to the next
  sub-element's path. The dispatcher's scope path shifts; no other
  state changes.
- **Commit**: write the cursor to the post-commit target (typically
  the newly-created or affected row); cascade-clear the widget's
  working state subtree with a `Null` write at its root.
- **Cancel**: read `cursor_saved`; cascade-clear the widget's
  working state subtree; restore the saved cursor.

#### Page-level bindings

Bindings that should fire whenever a sub-cursor is active but no
inner scope claims the key (`j`/`k` row navigation, `a` to open
compose, `?` for help, `Ctrl+S` for save) register at
`Exact(settings)` — the common ancestor of every cursor on the
settings screen. At Bubble phase they propagate to whichever cursor
is currently focused, after every inner scope has had a chance to
claim them at Target/Bubble.

## The View enum

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
    pub screen:     Screen,
    pub scope:      BindingScope,  // Anywhere | Exact(Path) | Prefix(Path)
    pub mode:       Option<Mode>,
    pub key:        KeyChord,
    pub command_id: CommandId,
    pub phase:      Phase,
}

pub enum Phase {
    Capture,
    Target,
    Bubble,
}
```

`Phase` is first-class and required at every registration — there is
no `Default` impl, so each `BindingEntry` constructed in Rust must
declare its phase explicitly. Phase is the routing decision the
dispatcher uses to walk the scope path; see "Hierarchical dispatch"
below.

`KeyChord` is a typed struct: `{ modifiers: KeyModifierSet, code:
KeyCodeRepr }`. `KeyCodeRepr` covers `Char(char)`, `Enter`, `Esc`,
`Tab`, `BackTab`, `Backspace`, `Delete`, `Up`, `Down`, `Left`,
`Right`, `PageUp`, `PageDown`, `Home`, `End`, `Insert`, `F(u8)`.

The text-editing scope (`settings/accounts/_detail`) gets ~96 bindings
from a helper that registers one `BindingEntry` per printable ASCII
char → `field.insert` at `Phase::Target`, plus one for `Backspace` →
`field.delete_back` at `Phase::Target`.

## Hierarchical dispatch

Phase is declared at registration on every `BindingEntry`. The
dispatcher walks the scope path through three phases generically; the
registry's `lookup` is phase-aware; there is no per-widget pass logic
in the dispatcher and no transitional Target-fallback shim.

A user keystroke routes through a *scope path* — the ordered chain of
nested scopes from the outermost (the page) to the innermost (the
focused leaf widget). The scope path **is the cursor's ancestor
chain**: every entry is the cursor (`ui/settings/focused`) walked
toward its root, wrapped in `BindingScope::Exact`. Each entry on the
path is either:

- a **page scope**: the cursor or one of its prefixes lying inside
  a registered page (`settings`, `settings/accounts`),
- a **compound widget scope**: a cursor prefix at a synthetic
  widget root (`settings/_compose_form`, `settings/_confirm_delete`,
  `settings/_manual_model`, `settings/_edit`),
- a **leaf scope**: the cursor's leaf segment when it points at a
  compound widget's sub-element (`settings/_compose_form/name`,
  `settings/_manual_model/id`). The same widget routes different
  keys at different leaves because each leaf is its own
  `BindingScope::Exact`.

The dispatcher walks the path in three phases, in order:

1. **Capture phase** — outermost-to-innermost, asking each scope for
   bindings registered with `phase: Capture`. First match fires and
   dispatch ends. Capture is for *lifecycle keys an outer scope
   always owns regardless of focus*: `Esc` to cancel a form, `Tab` to
   advance focus.

2. **Target phase** — only the innermost scope (the focused leaf) is
   consulted, for bindings with `phase: Target`. This is where the
   leaf claims its semantic keys: a text leaf claims printable ASCII;
   a selector leaf claims `h`/`l`. The same key can mean different
   things at different leaves because the binding lives on the leaf
   scope, not the form scope.

3. **Bubble phase** — innermost-to-outermost, asking each scope for
   bindings registered with `phase: Bubble`. First match fires.
   Bubble catches keys the leaf didn't claim. `Enter` on a compose
   form is bubble: a future multiline text field could bind `Enter`
   at target phase for newline insertion; the form's commit handler
   only fires if the leaf passed.

Lookup specificity within a single phase (most → least):

1. `scope: Exact + mode: Some`
2. `scope: Exact + mode: None`
3. `scope: Prefix(deeper) + mode: ...`
4. `scope: Prefix(shallower) + mode: ...`
5. `scope: Anywhere + mode: Some`
6. `scope: Anywhere + mode: None`

Ties broken by registration order.

### Scopes and the focus path

`compute_scope_path` is `ui/settings/focused`'s ancestor chain. The
dispatcher reads the cursor once per keystroke and walks its
ancestors outer-to-inner:

```rust
fn compute_scope_path(snap: &mut dyn Reader) -> Vec<BindingScope> {
    let Some(cursor) = read_cursor(snap) else { return Vec::new(); };
    path_ancestors(&cursor).into_iter().map(BindingScope::Exact).collect()
}
```

For cursor `settings/_compose_form/name` the path is `[settings,
settings/_compose_form, settings/_compose_form/name]`. The
dispatcher has no per-widget logic, no discriminator reads, no
synthetic-scope insertion. Mutual exclusion between compound
widgets is structural: the cursor is a single path, and only one
widget's synthetic prefix can be on its ancestry at a time.

The path is reconstructed per keystroke. No mutable
"currently-active-scope-stack" state lives anywhere; the snapshot is
the source of truth, and `ui/settings/focused` is the field within
the snapshot that determines it.

### `BindingScope::Anywhere` and the dispatch walk

`BindingScope::Anywhere` is the lowest-specificity scope tier and the
one place the dispatcher's scope-path walk does not name. Anywhere
bindings are **not** pushed onto `compute_scope_path`'s `Vec<BindingScope>`;
they ride into dispatch through the registry's specificity ordering
instead. Each per-phase `bindings.lookup(screen, scope, mode, key, phase)`
call considers every registered entry whose `scope.matches(cursor)`
returns true — and `BindingScope::Anywhere::matches` returns true for
*any* cursor. So at every scope the dispatcher visits in any phase,
Anywhere entries are candidates; the specificity sort
(`Exact > Prefix(deeper) > Prefix(shallower) > Anywhere`) keeps them
ranked last.

Practical effect: an Anywhere binding fires only when no more-specific
entry registered at the same phase claims the key. Within a given
phase, the walk produces the same outcome as if Anywhere had been
appended as an extra "outermost" scope; the registry collapses that
extra step into the per-scope query.

This is why the dispatcher code makes no mention of Anywhere. The
phase order — Capture (outer→inner), Target (leaf only), Bubble
(inner→outer) — is what callers reason about; Anywhere is a
specificity property of *individual bindings* layered on top, not a
fourth pass. An Anywhere+Capture binding is reachable on the first
Capture query at the outermost scope; an Anywhere+Bubble binding is
the final fallback at every Bubble query.

Convention for which phase an Anywhere binding should declare:

- **Lifecycle interceptors that should out-rank inner scopes** —
  declare `Phase::Capture`. The Anywhere+Capture binding fires at the
  outermost scope's Capture query, before any focused leaf sees the
  key. Reserved for keys with no per-screen meaning (e.g. a global
  panic-exit).
- **Ambient fallbacks that only fire when nothing else wants the key**
  — declare `Phase::Bubble`. The Anywhere+Bubble binding is queried
  last at every scope's Bubble pass, so `?` for help or `Ctrl+S` for
  save fire only when no inner leaf claims them.
- **`Phase::Target`** is rarely the right choice for Anywhere: Target
  only queries the leaf, so an Anywhere+Target binding only fires when
  the leaf has no Target binding for the key — which is brittle if the
  set of leaves grows. Prefer Bubble for "ambient" semantics.

### Worked example: typing `h` while composing

Snapshot state: cursor at `settings/_compose_form/protocol` (the
user opened compose and Tab'd onto the Selector field).

Scope path (outer → inner) — the cursor's ancestor chain:
- `settings`
- `settings/_compose_form`
- `settings/_compose_form/protocol`

Dispatch:
1. **Capture**: walk outer-to-inner. None of the scopes have `h`
   registered at Capture. (Capture-phase bindings on the form are
   `Esc`, `Tab`, `Shift+Tab`, `Up`, `Down`.)
2. **Target**: leaf scope's Target bindings include `h` →
   `accounts.compose.cycle_back`. Fires. Done.

Same keystroke, different focus: cursor at `settings/_compose_form/name`
(a Text field). The scope path's leaf swaps to the `name` scope:
1. **Capture**: no `h` at Capture on any scope.
2. **Target**: the `_compose_form/name` leaf has printable ASCII
   → `accounts.compose.insert_char`. Fires.

Same keystroke, no compose mode active: cursor at
`settings/accounts/<some-account>`. The `_compose_form` ancestors
are absent from the path:
1. **Capture**: no `h` at Capture on any scope.
2. **Target**: the leaf is the focused-row scope. `h` is not
   registered there for an accounts row.
3. **Bubble**: page-level bindings (`h`/`j`/`k`/`l` for navigation,
   focused-row `a`/`t`/`r`/`d`, whole-screen `?`) are registered at
   `Phase::Bubble` on `settings` — the common ancestor — and fire
   here. The dispatcher walks the path inner-to-outer at Bubble and
   the first registered match wins.

### When you'd add a new compound widget

Adding a modal, wizard, or inline form means:

1. Pick a synthetic cursor namespace for the widget
   (`settings/_my_widget`). The widget's "open" command writes the
   cursor there; the widget's "cancel"/"commit" commands restore
   the saved pre-open cursor.
2. Pick a UI-state subtree for the widget's working data
   (`ui/settings/my_widget/{buffer, cursor_saved, ...}`). The open
   command initializes it; the commit/cancel commands cascade-clear
   it via a `Null` write at the subtree root.
3. Register bindings under `Exact(settings/_my_widget)` for the
   widget-as-a-whole — lifecycle keys at Capture (Esc cancel, Tab
   advance) and form-commit at Bubble (Enter).
4. If the widget has multiple focusable children, the open command
   places the cursor on the first child's path
   (`settings/_my_widget/first_field`) and Tab/Shift+Tab commands
   move it among sibling paths. Register Target-phase bindings on
   each child scope.

No dispatcher changes are required. The dispatcher reads the cursor
and walks its ancestors; the new widget's scopes appear on the path
exactly when the cursor is inside them.

Anti-pattern: a single flat scope that does everything via
conditional logic inside command bodies. That works for a single
widget; it doesn't compose with siblings.

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
settings::dispatch::dispatch_settings_key(snapshot, screen, cursor,
                                          mode, key, cmds, bindings,
                                          renderers):
  // The cursor argument is the page cursor; the focus cursor lives
  // at `ui/settings/focused` and is read inside compute_scope_path.
  scope_path = compute_scope_path(snapshot)   // = focus.ancestors()
  // Capture: outer → inner
  for scope in &scope_path:
      if let Some(cmd) = bindings.lookup(screen, scope, mode, key,
                                         Phase::Capture):
          return commands.lookup(cmd).run(snapshot, &ctx)
  // Target: leaf only
  if let Some(leaf) = scope_path.last():
      if let Some(cmd) = bindings.lookup(screen, leaf, mode, key,
                                         Phase::Target):
          return commands.lookup(cmd).run(snapshot, &ctx)
  // Bubble: inner → outer
  for scope in scope_path.iter().rev():
      if let Some(cmd) = bindings.lookup(screen, scope, mode, key,
                                         Phase::Bubble):
          return commands.lookup(cmd).run(snapshot, &ctx)
  return vec![]  // inert; caller falls through to input-store path
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

Subscriptions are the *only* place async or cross-cutting work
happens. They are reactive observers of data-tree changes — not RPC
handlers. They watch path patterns; when a watched path changes,
they respond with side effects (HTTP fetches, multi-path cleanup,
file IO). They do not translate "please do X" sentinel writes into
"do X" data writes — that's the CLI's job, and it does the data
write directly.

Two shapes earn a subscription:

1. **Reactive observers**: watch a data-tree change and do follow-up.
   "When a new entry appears under `config/gate/accounts/`, fetch
   its catalog." `Prefix(config/gate/accounts)` watching new entries
   is the natural shape.
2. **Async action triggers**: the user requested work that can only
   happen asynchronously. `config/gate/accounts/<name>/test_now`
   carrying a `Null` write means "please run a connectivity test
   for this account." The trigger is legitimate because the action
   has no synchronous form.

Anti-pattern: the CLI writes a sentinel like
`config/gate/accounts/_create_now`, the subscription reads it,
validates, and writes the AccountConfig that the CLI could have
written directly. That's RPC indirection. The CLI should write the
AccountConfig itself; the subscription, if any, should react to the
new entry with whatever async follow-up is appropriate.

The protocol shape itself:

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

**Action-trigger path convention**: per-instance async actions live
at `<collection>/{id}/<verb>_now` (e.g.
`config/gate/accounts/<name>/test_now`). Writing `Null` to such a
path means "please perform this async action on this instance"; the
subscription handles it.

There are no *collection-level* trigger paths — a request to act on
the collection itself (e.g. "create a new account") is a write
*to the collection*, not a write to a sentinel sibling. The CLI
writes `config/gate/accounts/<name>` to create; a subscription
watching `Prefix(config/gate/accounts)` reacts if any async
follow-up is needed.

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
(by reading both data-tree and display-tree state and emitting a
`View` that reflects both), and impossible to desync from data (a
stale render only happens if you held onto a snapshot too long,
which the framework prevents by rebuilding it per frame).

**Commands pure** because the same argument applies and because
testing them is `assert_eq!` over `Vec<Write>`. A command that
spawned async work would be untestable.

**Subscriptions on the broker** rather than the renderer side because
they need to react to writes from anywhere — not just the user's
keypresses. A network test fires from the `t` key today, from an
auto-validation hook on save tomorrow, from a future `ox cli admin`
command later. All paths converge at the watched-pattern dispatch.

**Bindings as data** so v2 can let users edit them on disk. v1 ships
them hardcoded but the shape is forward-compat.

**Why the cursor is the universal focus authority.** Earlier
iterations split focus across discriminator paths
(`new_account/active: bool`, `manual_model/stage: ManualModelStage`,
`edit_mode: bool` + `edit_field_path`). Each new compound widget
brought its own discriminator and its own scope-insertion logic in
the dispatcher. The dispatcher held a small case analysis: "if
new_account/active, push the compose scope; if manual_model/stage
is set, push the manual-model scope at the right leaf; ..." Each
case multiplied the question of mutual exclusion — what happens if
both flags are accidentally set? — and pushed answers into the
write-side commands ("clear the other flags when you open").

Cursor-as-focus collapses all of that. The cursor is a single path;
its ancestor chain is the scope path; only one widget's prefix can
appear on its ancestry at a time. Mutual exclusion is structural,
not conventional. The dispatcher has no widget-specific code —
just `compute_scope_path` returning `cursor.ancestors()`. Adding a
new widget means picking a synthetic cursor namespace and writing
the open/cancel/commit handlers; no dispatcher edit, no new
discriminator, no new "what if both are set" branch.

The page-vs-widget distinction the framework still upholds is the
one users feel: page navigation (the `cursor` path) changes the
screen the user is reading; widget engagement (the `focused`
cursor descending into a synthetic `_<widget>` namespace) opens an
inline form on the same screen. Different verbs, different visual
shapes, same underlying mechanism: write a path, and the cursor's
new position determines the rest.

**Why the display tree names only real things.** Synthetic identifier
paths (`settings/accounts/_new`, `…/_delete`,
`settings/models/<account>/_empty`) put UI affordances and real
domain identifiers in the same namespace, dispatched by string
equality. That mostly works as long as no real domain identifier
ever collides with a synthetic — but the convention has to be
maintained at every write boundary in perpetuity, and a single
hand-edited TOML config can pierce it. Pulling synthetic affordances
out of the projection entirely (rendered by reading UI-mode state,
never as rows) makes the namespace invariant structural rather than
conventional. Path-equality dispatch becomes safe by construction.

**Why a write IS the action.** The substrate already provides the
verb. Wrapping a state change in a "request → subscription → state
change" round-trip adds a name (the sentinel) and a translation
layer (the subscription handler) without adding capability. When
the subscription is doing real work (HTTP, multi-path cleanup), the
indirection is justified by the work. When the subscription is
*just translating*, it's pure overhead. The path-MVU model works
because writes mean what they say; sentinel-as-RPC undermines that.
