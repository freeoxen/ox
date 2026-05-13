# UI framework

A path-based MVU UI framework. The settings screen is its first user; the
inbox/threads screens will move onto it as they get rebuilt.

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

Every piece of UI state — including which page you're looking at —
lives at a path in StructFS. Renderers are pure `&mut dyn Reader →
View` functions, registered against cursor paths. Commands are pure
`&mut dyn Reader, &CommandCtx → Vec<Write>` functions, registered by
id. Bindings (`(screen, scope, mode, key, phase) → CommandId`) route
key events through a three-phase walk (capture → target → bubble)
over the scope path. **A write to a data-tree path IS the action that
path represents.** Subscriptions exist only for async or
cross-cutting follow-up work; they never wrap a write the CLI could
have made directly.

The View enum is small and curated. The translator (the only place
ratatui is touched) is total over it. Adding a "widget" requires
extending the enum *and* the translator — that cost is the point.

## Four architectural commitments

These are the design principles the rest of the framework derives
from. If you find yourself fighting one, the answer is almost always
to revisit your design — not to work around the principle.

### 1. A write IS the action

A direct write to a data-tree path is the action that path
represents. Creating an account is a write of `AccountConfig` to
`config/gate/accounts/<name>`. Deleting is a `Null` write to the same
path. Renaming is a delete plus a write. The CLI's command handlers
perform these writes themselves.

Subscriptions are reactive observers, not RPC handlers. They watch
path patterns and respond with *async* or *cross-cutting* follow-up:
HTTP fetches, multi-path cleanup, file IO. They do not translate
"please do X" sentinel writes into "do X" data writes — the CLI does
the data write directly, and the subscription, if any, fires off the
side effects in response.

Trigger paths like `config/gate/accounts/<name>/test_now` remain
legitimate when the trigger represents work that *can only* happen
asynchronously (running a network test). The shape is: synchronous
writers produce data; subscriptions watch data and produce side
effects. RPC indirection through sentinel paths is the anti-pattern.

### 2. Mode is state, not place

A *cursor scope* is a page you navigate to. A *mode* is a state the
user is in within a page. They are different things.

Modes live at named UI-state paths as ordinary values:
- `ui/settings/new_account/buffer: Option<String>` — when present,
  the user is composing a new account name.
- `ui/settings/pending_delete: Option<AccountName>` — when present,
  the user is being asked to confirm a delete.
- `ui/settings/edit_buffer: Option<String>` + `edit_field_path:
  Option<Path>` — when both present, an existing field is being
  edited inline.

The renderer reads these values and decorates the page accordingly.
The dispatcher reads them and routes keys accordingly. Neither
requires navigating to a special cursor scope.

Cursor scopes are reserved for true page navigation: `settings/index`,
`settings/accounts`, `settings/models`. There are no `…/_new`,
`…/_delete`, `…/_edit` cursor scopes — those would be modes
masquerading as places.

### 3. The display tree names only real things

Every path in the display tree (`settings/…`, `ui/…`) names either a
real thing in the data tree or a UI-state value with semantic
meaning. Synthetic affordances — "+ New connection", "no models —
refresh", "+ add model manually" — are renderer-side decorations
reading UI-mode state. They are not rows in the visible-rows
projection and they do not have synthetic identifier paths.

The visible-rows projection is a pure function from the data tree to
real-account rows. The `+ New connection` line still appears in the
rendered output, but it appears because the renderer reads
`ui/settings/new_account/buffer` and emits an inline prompt or a
static affordance line accordingly — not because a synthetic row
exists in the projection.

This is what makes path-equality dispatch safe: every path in
`settings/accounts/<name>` names a real account, full stop. There is
no `settings/accounts/_new` competing for the same namespace.

### 4. Input dispatch is hierarchical

The widget hierarchy IS the dispatch hierarchy. A focused leaf widget
sits inside a compound widget, which sits inside a page, which sits
inside the screen. Each level is a *scope* with its own bindings.

A keypress walks the scope path from outer to inner (the **capture
phase**), arrives at the focused leaf (the **target phase**), and any
unclaimed keystroke bubbles back outer-to-inner becomes inner-to-outer
(the **bubble phase**). This mirrors DOM event flow. Each scope on
the path gets one chance per phase to claim the key; first match wins.

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
3. **The translator is dumb.** It pattern-matches on the View enum.
   It never inspects a *value* to decide *which* widget to draw.
4. **All async lives in subscriptions.** A user-pressed `t` becomes a
   write to `…/test_now`; the subscription does the network call.
5. **All paths are constructed via `oxpath!` or
   `PathComponent::try_new`.** Never hand-format path strings.
6. **No synthetic display paths.** Every path in the display tree
   names a real thing in the data tree or a UI-state value with
   semantic meaning. If you find yourself reaching for `…/_foo` as
   "the place where the user is doing X," X is a *mode* — model it
   as state at a named path instead.
7. **Bindings declare their phase, not their disambiguation.** A
   binding fires at capture, target, or bubble — chosen by *which
   scope owns it*, not by what key it is. If you find yourself adding
   conditional logic inside a command to handle "depending on focus,
   either type this character or cycle the selector," stop — that's
   two bindings, one per scope, distinguished by phase.

If you find yourself writing async code in a renderer or command,
stop — you're building the wrong shape. Move the effect into a
subscription. If you find yourself writing a subscription that
*translates* a sentinel write into a data write, stop — the CLI
should make that data write directly. If you find yourself adding a
synthetic row to the projection to drive a UI affordance, stop —
that's a mode, not a row.

## Branch / SHA

Framework landed on branch `improvements`, commits `5b97d63` (the
first A0 commit) through `8dba6d9` (Phase S cleanup). About 50 commits
in 19 phases. Hierarchical-dispatch generalization (commitment #4)
landed on the same branch as a follow-up series.
