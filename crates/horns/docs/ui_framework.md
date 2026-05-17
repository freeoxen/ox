# horns — path-MVU UI toolkit

A reusable path-based MVU UI framework. State lives at paths in a
StructFS broker; renderers and commands are pure functions over a
Reader; dispatch is hierarchical along the focus cursor's ancestor
chain. horns is installed as a broker mount — the host calls
`horns::install(broker, options)` once per screen and thereafter
interacts with the framework only by writing to broker paths.

The ox CLI's settings screen is the first user and appears throughout
this documentation as a worked example. The framework itself is
screen-agnostic; nothing in horns-core depends on settings.

This index page is the only thing every reader needs. The rest is split
so you can read just what's relevant to your task.

## When to read what

| You are... | Read |
|---|---|
| First-time reader | This file (60 seconds), then `architecture.md` |
| Adding a screen, command, binding, or subscription | `howto.md` |
| Looking up a type signature, path, or filename | `reference.md` |
| Curious about why it's shaped this way | `architecture.md` §Why |

Files:

- `ui_framework/architecture.md` — mental model (three trees, the
  cursor, dispatch flow, the snapshot). 5-minute read.
- `ui_framework/howto.md` — task-oriented recipes. Copy-paste shapes
  for the work you're doing.
- `ui_framework/reference.md` — type signatures, paths, file map,
  glossary. Lookup-only.

## 60-second pitch

horns is a *mount* on a StructFS broker, not a library you call. The
host calls `horns::install(broker, options)` exactly once per logical
screen at startup. `install` registers three subscriptions on the
broker:

- **KeyDispatch** watches the configured input path. Each write
  fires the dispatcher.
- **Render** watches a render-tick path that the dispatcher bumps
  after each cascade. Each write walks the renderer registry and
  writes the resulting View to the configured view-output path.
- **ThemeChange** watches the theme path and bumps the render tick
  so the screen re-renders with the new palette.

After install, the host's only interface is broker writes: write a
`KeyChord` to the input path; the dispatcher fires; the matched
command or handler produces writes; the broker cascades them; the
renderer fires; the backend draws.

Every piece of UI state — including which page is focused — lives at
a path. Renderers are pure `&mut dyn Reader → View` functions,
registered against cursor paths. Commands are pure `&mut dyn Reader,
&CommandCtx → Vec<Write>` functions, registered by id. Bindings
(`(scope, key, phase) → CommandId`) route key events through a
three-phase walk (capture → target → bubble) over the cursor's
ancestor chain. **A write to a data-tree path IS the action that
path represents.** Subscriptions exist only for async or
cross-cutting follow-up work; they never wrap a write the host could
have made directly.

The View enum is small and curated. The backend (horns-ratatui, or a
future horns-web / horns-iced) is total over it. Adding a "widget"
requires extending the enum *and* every backend — that cost is the
point.

## Four architectural commitments

These are the design principles the rest of the framework derives
from. If you find yourself fighting one, the answer is almost always
to revisit your design — not to work around the principle.

### 1. A write IS the action

A direct write to a data-tree path is the action that path
represents. Creating a record is a write of the typed value to its
canonical path. Deleting is a `Null` write to the same path.
Renaming is a delete plus a write. Host command handlers perform
these writes themselves.

In the settings worked example, creating an account is a write of
`AccountConfig` to `config/gate/accounts/<name>`, and deleting is a
`Null` write to the same path. The pattern generalizes — every
data-tree mutation a host needs is shaped the same way.

Subscriptions are reactive observers, not RPC handlers. They watch
path patterns and respond with *async* or *cross-cutting* follow-up:
HTTP fetches, multi-path cleanup, file IO. They do not translate
"please do X" sentinel writes into "do X" data writes — the host does
the data write directly, and the subscription, if any, fires off the
side effects in response.

Trigger paths like `<record>/<verb>_now` remain legitimate when the
trigger represents work that *can only* happen asynchronously
(running a network test, fetching a catalog). The shape is:
synchronous writers produce data; subscriptions watch data and
produce side effects. RPC indirection through sentinel paths is the
anti-pattern.

### 2. The cursor is the universal focus authority

The focus cursor (a path stored at the host's configured
`<cursor_path>`, e.g. `ui/<screen>/focused`) is the single source of
truth for what is currently focused. Its ancestor chain is the scope
path the dispatcher walks. Three distinct cases share one mechanism:

- Cursor at a row path (`<page>/<row>`) → that row is focused; no
  compound widget is engaged.
- Cursor at a compound widget root (`<page>/_<widget>`) → the
  widget is engaged as a whole.
- Cursor at a compound widget sub-element
  (`<page>/_<widget>/<leaf>`) → that sub-element is focused; the
  widget is active by virtue of being on the cursor's ancestors.

Compound widgets (forms, confirmations, inline edits, multi-stage
prompts) live under synthetic cursor namespaces (`<page>/_<widget>`).
The widget's working state — the typed buffer, the saved pre-open
cursor, staged drafts — lives at sibling UI-state paths under a
configured working-state prefix (e.g.
`<ui-state-prefix>/<widget>/buffer`,
`<ui-state-prefix>/<widget>/cursor_saved`). The cursor's position
is the *discriminator*; the UI-state subtree is the *data*.

A host that distinguishes "the page the user is reading" from
"what's focused within that page" can use two cursors — one for page
navigation, one for focus — but the framework only requires the
focus cursor. Esc on a widget restores the saved cursor; Esc on a
page ascends to the parent. These are different verbs in the user's
head and they remain different verbs in the framework.

### 3. The display tree names only real things

Every path in the display tree names either a real thing in the data
tree or a UI-state value with semantic meaning. Synthetic affordances
— "+ New X", "no items — refresh", "+ add manually" — are
renderer-side decorations reading UI-state. They are not rows in the
visible-rows projection and they do not have synthetic identifier
paths.

The visible-rows projection is a pure function from the data tree to
real rows. Affordance lines still appear in the rendered output, but
they appear because the renderer reads UI-state (e.g. a compose
buffer) and emits an inline prompt or a static affordance line
accordingly — not because a synthetic row exists in the projection.

This is what makes path-equality dispatch safe: every path under
`<page>/<row>` names a real record, full stop. There is no
`<page>/_new` competing for the same namespace.

### 4. Input dispatch is hierarchical

The widget hierarchy IS the dispatch hierarchy. A focused leaf widget
sits inside a compound widget, which sits inside a page, which sits
inside the screen. Each level is a *scope* with its own bindings.

A keypress walks the scope path from outer to inner (the **capture
phase**), arrives at the focused leaf (the **target phase**), and
any unclaimed keystroke bubbles inner-to-outer (the **bubble
phase**). This mirrors DOM event flow. Each scope on the path gets
one chance per phase to claim the key; first match wins.

Why three phases instead of just "innermost-first":
- **Capture-phase bindings** belong to the outer scope and are claimed
  *before* the focused leaf sees the key. Lifecycle keys (Esc to
  cancel a form, Tab to advance focus) belong here — they should fire
  regardless of which child has focus.
- **Target-phase bindings** are owned by the focused leaf. A text
  field claims printable ASCII; a selector field claims `h`/`l`. The
  same key has different meaning at different leaves.
- **Bubble-phase bindings** on the outer scope catch everything the
  leaf didn't. `Enter` on a form is bubble-phase: a multiline text
  field could plausibly bind `Enter` to "insert newline," and the
  form's "commit" handler should only fire if the field passed.

The pattern that emerges: a compound widget declares
`capture` / `bubble` bindings on its own scope and picks the active
child scope by reading focus state. The dispatcher walks the path
without the widgets knowing about each other.

Anti-pattern: a single flat binding table that requires every key to
declare its full disambiguation upfront. Two widgets that both want
to bind `h` (one as text input, one as cycle-back) cannot coexist in
a flat table without explicit conditional bindings — but they coexist
naturally when the leaf binds `h` at target phase and the outer scope
binds `h` at neither phase.

## Seven invariants you must keep

1. **Renderers are pure.** No async, no I/O, no mutation. Take a
   `&mut dyn Reader`, return a `View`.
2. **Commands are pure.** Take a snapshot + `CommandCtx`, return
   `Vec<Write>`. No spawning, no awaiting, no global state mutation.
3. **The backend is dumb.** It pattern-matches on the View enum.
   It never inspects a *value* to decide *which* widget to draw.
4. **All async lives in subscriptions.** A user-triggered async
   action becomes a write to a `…/<verb>_now` trigger path; the
   subscription does the network call.
5. **All paths are constructed via `oxpath!` or
   `PathComponent::try_new`.** Never hand-format path strings.
6. **No synthetic identifier paths in the visible-rows
   projection.** Real data rows (`<page>/<row>`) and UI affordances
   ("+ New X", "no items — refresh") live in different namespaces.
   Compound widgets have synthetic *cursor* namespaces
   (`<page>/_<widget>`) — those are where the focus cursor lands
   when the widget is engaged, not identifiers for projected rows.
   Never put a `…/_foo` path into the visible-rows projection.
7. **Bindings declare their phase, not their disambiguation.** A
   binding fires at capture, target, or bubble — chosen by *which
   scope owns it*, not by what key it is. If you find yourself adding
   conditional logic inside a command to handle "depending on focus,
   either type this character or cycle the selector," stop — that's
   two bindings, one per scope, distinguished by phase.

If you find yourself writing async code in a renderer or command,
stop — you're building the wrong shape. Move the effect into a
subscription. If you find yourself writing a subscription that
*translates* a sentinel write into a data write, stop — the host
should make that data write directly. If you find yourself adding a
synthetic row to the projection to drive a UI affordance, stop —
the renderer should read UI-state and decorate the section
directly. If you find yourself adding a new `…/active: bool` flag
or `…/stage: SomeEnum` discriminator to tell the dispatcher which
compound widget is engaged, stop — move the cursor into the
widget's synthetic namespace and let the dispatcher's cursor-
ancestor walk pick up the scope automatically.
