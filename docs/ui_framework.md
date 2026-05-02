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
id. Bindings (`(screen, cursor, mode, key) → CommandId`) route key
events. Long-running effects are subscriptions on the broker, watching
path patterns.

The View enum is small and curated. The translator (the only place
ratatui is touched) is total over it. Adding a "widget" requires
extending the enum *and* the translator — that cost is the point.

## Five invariants you must keep

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

If you find yourself writing async code in a renderer or command,
stop — you're building the wrong shape. Move the effect into a
subscription.

## Branch / SHA

Framework landed on branch `improvements`, commits `5b97d63` (the
first A0 commit) through `8dba6d9` (Phase S cleanup). About 50 commits
in 19 phases.
