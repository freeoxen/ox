# TextInput System Design

**Date:** 2026-04-11
**Status:** Approved

## Problem

The ox TUI input has three UX problems:

1. **Paste is slow** — no bracketed paste mode; each pasted character is a separate crossterm event → separate async broker write → separate ViewState fetch → separate full redraw. O(n) for n characters.
2. **Newlines don't render** — Enter inserts `\n` correctly but the input renders as a single-line `Paragraph` with cursor math that assumes one line.
3. **No multiline editing** — no scroll, no cursor movement across lines, no word wrap.

The deeper architectural issue: every keystroke round-trips through the async broker before anything renders. The input state (`String` + `usize`) is buried in UiStore with ad-hoc mutation commands (`insert_char`, `delete_char`). No clean protocol for other frontends (web, remote) to share.

## Design

Three components:

1. **InputStore** (ox-ui) — broker-side store with a defined edit protocol
2. **InputSession** (ox-cli event loop) — optimistic local state + edit buffer
3. **TextInputView** (ox-cli) — pure renderer, string + cursor → Rect

### Architecture

```
┌─────────────────────────────────────────────────┐
│ Event Loop (ox-cli)                             │
│                                                 │
│  crossterm events                               │
│       │                                         │
│       ▼                                         │
│  keybinding resolution (broker)                 │
│       │                                         │
│       ├─ "edit" ──► InputSession                │
│       │              ├─ apply locally (instant)  │
│       │              └─ buffer Edit              │
│       │                                         │
│       ├─ "submit" ─► flush edits, send_input    │
│       └─ "command" ─► broker directly            │
│                                                 │
│  end of tick:                                   │
│    pending_edits ──► async write ui/input/edit   │
│    InputSession.content ──► TextInputView.render │
└─────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────┐
│ Broker                                          │
│                                                 │
│  UiStore routes input/* ──► InputStore           │
│    ├─ read: content, cursor, "" (snapshot)       │
│    └─ write: edit (sequence of Edit structs)     │
│                                                 │
│  InputStore is the canonical source of truth.    │
│  Any frontend (CLI, web, remote) can connect.    │
└─────────────────────────────────────────────────┘
```

### Sync Model

Optimistic local write, broker-wins on discontinuity. Same mental model as a web editor with server sync:

- The client (InputSession) applies edits locally for instant rendering.
- Every edit is forwarded to the broker asynchronously (non-blocking).
- The broker is the canonical store. If the broker's state diverges from local (reconnect, remote edit, server-side transform), the client replaces its state wholesale with the broker's version.
- No OT, no CRDT, no merge. Broker wins.

### InputStore (ox-ui)

A StructFS store holding the canonical input state. Plain Rust, no TUI dependencies.

**State:**

```rust
pub struct InputStore {
    content: String,
    cursor: usize,
}
```

**Read paths:**

| Path | Returns |
|------|---------|
| `""` | `Value::Map { content, cursor }` — full snapshot for client init |
| `content` | `Value::String` |
| `cursor` | `Value::Integer` |

**Write paths:**

| Path | Payload | Effect |
|------|---------|--------|
| `edit` | Edit sequence (see below) | Apply edits in order, update content + cursor |
| `replace` | `{ content, cursor }` | Wholesale replace (server-initiated) |
| `clear` | — | Reset to empty string, cursor 0 |

**Edit sequence format:**

A single write to `edit` carries a sequence of edits, each preserving timing and source:

```json
{
    "edits": [
        { "op": "insert", "text": "h", "at": 42, "source": "key", "ts_ms": 1000 },
        { "op": "insert", "text": "e", "at": 43, "source": "key", "ts_ms": 1032 },
        { "op": "delete", "at": 44, "len": 1, "source": "key", "ts_ms": 1100 }
    ],
    "generation": 7
}
```

**Edit fields:**

| Field | Type | Description |
|-------|------|-------------|
| `op` | `"insert"` or `"delete"` | The operation |
| `text` | String (insert only) | Text to insert |
| `at` | usize | Byte offset in content |
| `len` | usize (delete only) | Number of bytes to delete |
| `source` | `"key"`, `"paste"`, `"completion"`, `"replace"` | How the edit originated |
| `ts_ms` | u64 | Monotonic timestamp in milliseconds |

**`source` semantics:**

- `key` — single keystroke. Debounce candidate, may trigger autocomplete.
- `paste` — bracketed paste. Large text, don't trigger per-character features.
- `completion` — autocomplete/suggestion accepted.
- `replace` — programmatic replacement (discontinuity recovery).

**`generation`** — monotonic counter from the client. Incremented on discontinuity. The server can discard stale writes from a previous generation.

**Server-side capabilities enabled by this protocol:**

- Debounce rapid keystrokes into logical words (cluster by timing gaps)
- Distinguish typing from paste from autocomplete
- Detect idle (gap in timestamps → save draft, trigger analysis)
- Replay exact input timeline for debugging
- Batch timing analysis for shortcut disambiguation

**Mounting:**

UiStore mounts InputStore and routes `input/*` to it:

```rust
"input" => Some((&mut self.input_store as &mut dyn Store, sub))
```

### InputSession (ox-cli event loop)

Optimistic local state held in the event loop. Not a separate struct initially — just fields in the event loop's state, or a small struct if cleaner.

```rust
struct InputSession {
    content: String,
    cursor: usize,
    pending_edits: Vec<Edit>,
    generation: u64,
}
```

**Per render tick:**

1. Drain all available crossterm events from the queue (don't process one at a time).
2. For each event, resolve keybinding via broker.
3. If edit: apply to `content`/`cursor` locally, push `Edit` to `pending_edits`.
4. If command (submit, cancel, etc.): flush `pending_edits` to broker (await), then execute command.
5. After all events processed: if `pending_edits` is non-empty, fire async write to `ui/input/edit`. Non-blocking.
6. Pass `content` + `cursor` to `TextInputView` for rendering.

**On discontinuity:**

Broker pushes new state (via a read that doesn't match local, or an explicit replace signal). InputSession:
- Overwrites `content` and `cursor` with broker values
- Clears `pending_edits`
- Increments `generation`

**On submit:**

1. Flush `pending_edits` to broker (await confirmation)
2. Write `ui/send_input` to broker
3. Broker handles submission, may clear input via replace

**On focus/open:**

Async read `ui/input` from broker → populate `content` + `cursor`. If broker has a draft from a previous session, it appears.

### TextInputView (ox-cli)

Pure renderer. ~80 lines. No event handling, no buffer ownership, no tui-textarea dependency.

```rust
pub struct TextInputView {
    content: String,
    cursor: usize,
    scroll_top: usize,
}
```

**Methods:**

- `set_state(&mut self, content: &str, cursor: usize)` — called each frame from InputSession's optimistic state.
- `render(&mut self, frame: &mut Frame, area: Rect)` — draws text with wrap, positions cursor, manages scroll.
- `ensure_cursor_visible(&mut self, area_height: u16)` — adjusts `scroll_top` so the cursor line is in the viewport.

**Rendering internals:**

- Split content into display lines (hard breaks on `\n`, soft wrap at area width)
- Use ratatui `Paragraph` with `Wrap { trim: false }` for soft wrapping
- Compute cursor (line, col) by walking content up to byte offset
- Adjust for scroll: offset the Paragraph content by `scroll_top` lines
- `frame.set_cursor_position()` with line and column accounting for scroll offset
- Scroll management: if cursor line < `scroll_top`, scroll up. If cursor line >= `scroll_top` + viewport height, scroll down.

### Bracketed Paste

Enable in `main.rs`:

```rust
crossterm::execute!(
    std::io::stdout(),
    crossterm::event::EnableMouseCapture,
    crossterm::event::EnableBracketedPaste,
);
```

Cleanup on exit:

```rust
crossterm::execute!(
    std::io::stdout(),
    crossterm::event::DisableBracketedPaste,
    crossterm::event::DisableMouseCapture,
);
```

`Event::Paste(text)` in the event loop → single `Edit { op: insert, text, source: paste, ts_ms }`. Applied locally as one operation, sent to broker as one edit in the sequence.

### Migration

1. Add `InputStore` to `ox-ui` with Edit types and StructFS Reader/Writer impl.
2. Mount `InputStore` in UiStore routing under `input/*`.
3. Remove `input: String`, `cursor: usize` from UiStore. Remove `insert_char`, `delete_char`, `set_input` commands.
4. Add `TextInputView` to `ox-cli`.
5. Add `InputSession` to event loop. Rework event dispatch: drain events per tick, buffer edits, batch write to broker.
6. Handle `Event::Paste` in event loop.
7. Enable bracketed paste in `main.rs`.
8. Update `tui.rs` rendering: use `TextInputView` instead of `Paragraph::new(format!("> {}", vs.input))`.
9. Update ViewState to read input from `ui/input` instead of UiStore's input fields.

### Testing

- **InputStore unit tests** (ox-ui): edit sequence application, cursor management, generation handling, insert/delete at boundaries, empty content edge cases.
- **TextInputView unit tests** (ox-cli): cursor-to-line-col conversion, scroll management, wrap calculation. Can test with known content + area dimensions.
- **Integration**: manual testing of paste speed, multiline editing, cursor movement, submit flow.

### What this does NOT include

- Selection / highlighting (future: add selection range to InputStore + rendering in TextInputView)
- Undo/redo (future: InputStore can maintain an edit history from the sequence)
- Autocomplete / ghost text (future: InputStore analyzes edit patterns, TextInputView renders suggestions)
- tui-textarea (explicitly excluded — we build the renderer ourselves)

These are all enabled by the protocol but not implemented in this phase.
