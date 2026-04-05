# ox-cli Inbox TUI: Modal System + Visual Rendering

**Date:** 2026-04-05
**Status:** Draft
**Depends on:** Plan 2a (multi-thread state + AgentPool — committed)

## Overview

Add vim-style modal input, an inbox thread list view, and visual
differentiation between views to ox-cli. The input box only appears in Insert
mode, giving Normal mode the full screen for content. Search uses live filtering
with compounding numbered filter chips.

## Modes

Three mutually exclusive input modes:

### Normal Mode

Full-screen content, no input box. Key bindings depend on which view is active:

**Inbox view:**
- `j`/`k` or `Up`/`Down` — navigate thread list
- `Enter` — open selected thread as tab
- `d` — archive selected thread (mark done)
- `i` — enter Insert mode (compose new thread)
- `/` — enter Insert mode with search context (live filter)
- `1`–`9` — dismiss numbered filter chip
- `Esc` — do nothing (already at top level)
- `Ctrl+C` — quit

**Thread view:**
- `j`/`k` or `Up`/`Down` — scroll conversation
- `i` — enter Insert mode (reply)
- `y`/`n`/`s`/`a`/`d`/`c` — approval quick keys (when approval pending)
- `Esc` — go to inbox view
- `Ctrl+W` — close tab (thread keeps running)
- `Ctrl+Right`/`Ctrl+Left` — switch tabs
- `Ctrl+T` — go to inbox

### Insert Mode

Input box appears at bottom with blue accent top border. Draft persists across
mode switches (Esc exits Insert without discarding).

**Key bindings (all views):**
- `Enter` — newline (prompts are often multiline)
- `Ctrl+Enter` — send
- `Ctrl+S` — send
- `Esc` — exit to Normal mode (draft preserved). In Normal mode, `Enter` sends
  the buffered draft if one exists; otherwise `Enter` performs its Normal mode
  action (open thread, etc.).
- `Ctrl+U` — clear input
- `Ctrl+A`/`Ctrl+E` — start/end of line
- `Backspace`/`Left`/`Right` — editing

**Context determines send behavior:**
- Insert mode entered via `i` in inbox → send creates a new thread (agent starts
  running, user stays in inbox view)
- Insert mode entered via `i` in thread tab → send replies to that thread
- Insert mode entered via `/` → live search mode (see Search section)

### Approval Mode

Existing permission dialog, unchanged. Triggered by the agent requesting tool
approval. Auto-switches to the requesting thread's tab.

## Views

### Inbox View

A dense thread list occupying the full screen (minus tab bar and status bar).

**2-line rows:**
```
● BLOCKED  Refactor auth middleware                    3m
  [backend]  Approval: shell "cargo test" · ☑ 3/5 · 12.4k tok
```

Line 1: state dot (colored) + state label + title (bold for active, dim for
completed) + recency (right-aligned).

Line 2: label chips + current activity or completion summary + task progress +
token count.

**State colors:**
- RUNNING — green
- BLOCKED — orange
- WAITING — purple
- COMPLETED — dim/gray
- ERRORED — red

**Selected row** has a subtle background highlight. The cursor (selected row
index) wraps at list boundaries.

**Default sort:** urgency (blocked > errored > waiting > running > completed),
then recency within each group.

### Thread View

Existing conversation rendering — user prompts, assistant text, tool calls, tool
results, error messages. Fills the full screen (no input box in Normal mode).

Approval banners render inline in the conversation with an orange left border.

### Compose

Not a separate view. Composing a new thread is: press `i` in inbox → input box
appears at bottom of inbox view → type prompt → send → input box disappears,
new thread appears in list as RUNNING, user stays in inbox.

## Search

Live-filtering search with compounding numbered filter chips.

**Flow:**
1. `/` in inbox Normal mode → Insert mode with search context
2. Type query → inbox filters live as you type (matches title, labels, state)
3. `Ctrl+Enter`/`Ctrl+S` → save current text as a numbered filter chip, stay in
   search Insert mode for another fragment
4. `Esc` → exit search to Normal mode, chips persist

**Filter bar (visible when chips exist or in search mode):**
```
/ [1: backend] [2: blocked] auth_
```

Saved chips `[1: backend]` and `[2: blocked]` are AND-compounded. `auth_` is
the live fragment narrowing further.

**Dismissing chips:** In Normal mode, press `1`–`9` to dismiss the
corresponding chip. All chips dismissed → back to unfiltered view, filter bar
hidden.

**Search scope:** unified text match across thread title, labels, and thread
state name.

## Tab Bar

Top row, always visible.

- Inbox tab always first, shows thread count: `■ Inbox (4)`
- Open thread tabs show truncated title
- Active tab has highlighted background + colored text
- Inactive tabs are dimmed

## Status Bar

Bottom row, always visible. Three sections:

1. **Mode badge:** `NORMAL` (blue) or `INSERT` (orange) — always leftmost
2. **Context info:** thread count + attention count (inbox) or thread state +
   task progress + tokens (thread view)
3. **Key hints:** mode-appropriate, right-aligned. Change dynamically based on
   mode + view.

## File Structure

Split the current monolithic `tui.rs` (883 lines) into focused modules:

| File | Responsibility |
|------|---------------|
| `tui.rs` | Event loop, mode dispatch, top-level draw routing |
| `inbox_view.rs` | Inbox thread list rendering, thread row widget |
| `thread_view.rs` | Conversation rendering (extracted from current tui.rs) |
| `tab_bar.rs` | Tab bar widget |

Add to `app.rs`:
- `InputMode` enum: `Normal`, `Insert { context: InsertContext }`, `Approval`
- `InsertContext` enum: `Compose`, `Reply`, `Search`
- `SearchState`: `chips: Vec<String>`, `live_query: String`
- Selected inbox row index

## V1 Scope

### In Scope

- InputMode system (Normal/Insert/Approval) with mode-aware key dispatch
- Inbox view with 2-line thread rows, state colors, selection
- Thread view adapted for modal system (no input box in Normal)
- Tab bar rendering
- Compose flow (stays in inbox after send)
- Live search with compounding filter chips
- Status bar with mode badge + context hints
- tui.rs file split

### Deferred

- Thread grouping by label (show label headers in inbox)
- Inline thread preview (expand thread details without opening tab)
- Keyboard-driven label assignment
- Mouse click on thread rows
- Snooze / pin
