# History Explorer Design

**Date:** 2026-04-13
**Status:** Draft
**Goal:** Add a full-screen history explorer to ox-cli, accessible via `h` in normal mode on a thread, providing real-time structured inspection of the message log with drill-down capability.

## Motivation

When debugging issues like the looping output bug (where LLM responses repeat in the rendered thread), there's no way to inspect what's actually in the history store. The thread view renders a processed/formatted version of messages. The history explorer shows the raw structured data — each message, its role, content blocks, and metadata — so patterns like duplicates or malformed entries are immediately visible.

## Architecture

### New Screen Variant

Add `History` as a fourth screen type alongside Inbox, Thread, and Settings.

**ox-types changes:**

- `Screen` enum: add `History` variant
- `ScreenSnapshot` enum: add `History(HistorySnapshot)` variant
- `UiCommand` enum: add `History(HistoryCommand)` scope
- New `HistorySnapshot` struct (thread_id, selected_row, scroll, expanded set)
- New `HistoryCommand` enum (navigation, expand/collapse)

```rust
// snapshot.rs
pub struct HistorySnapshot {
    pub thread_id: String,
    pub selected_row: usize,
    pub scroll: usize,
    pub scroll_max: usize,
    pub viewport_height: usize,
    /// Set of message indices currently expanded for detail view.
    pub expanded: Vec<usize>,
}

// command.rs
pub enum HistoryCommand {
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    ToggleExpand,
    ExpandAll,
    CollapseAll,
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollHalfPageUp,
    ScrollHalfPageDown,
    SetScrollMax { max: usize },
    SetViewportHeight { height: usize },
}
```

### Screen Transition

**Entry:** `GlobalCommand::OpenHistory { thread_id }` transitions from the thread screen to the history screen, carrying the thread_id. Triggered by `h` key binding in normal mode on the thread screen.

**Exit:** `GlobalCommand::Close` (or a new `GlobalCommand::BackToThread { thread_id }`) returns to the thread screen for the same thread. Triggered by `Esc`, `q`, or `h` (toggle behavior).

Since `GlobalCommand::Close` currently always goes to inbox, we need a `BackToThread { thread_id }` variant so that closing the history explorer returns to the thread, not the inbox.

### Navigation Command: `open_history`

Add an `open_history` command to the command registry. The `h` key binding on the thread screen in normal mode invokes this command. The command reads the current thread_id from UiSnapshot and dispatches `GlobalCommand::OpenHistory { thread_id }`.

### Key Bindings (bindings.rs)

```
Normal mode, screen: history
  j / Down        → select_next        (move cursor to next message)
  k / Up          → select_prev        (move cursor to previous message)
  g               → select_first       (jump to first message)
  G               → select_last        (jump to last message)
  Enter / Space   → toggle_expand      (expand/collapse selected message)
  e               → expand_all         (expand all messages)
  E               → collapse_all       (collapse all messages)
  Ctrl+d          → scroll_half_page_down
  Ctrl+u          → scroll_half_page_up
  Ctrl+f          → scroll_page_down
  Ctrl+b          → scroll_page_up
  Esc / q / h     → back_to_thread     (return to thread view)
  ?               → shortcuts           (show key help)

Normal mode, screen: thread (new binding)
  h               → open_history       (open history explorer)
```

## Data Flow

### Reading History

The history explorer reads from the same broker path as the thread view:
`threads/{thread_id}/history/messages` returns a `Value::Array` of wire-format Anthropic messages.

`fetch_view_state` already conditionally reads based on screen type. Add a `ScreenSnapshot::History` arm that reads from this path, identical to the thread arm. Since the event loop re-fetches state every 50ms tick, the view updates in real time as the agent streams responses.

### Parsed Representation

Reuse the existing `parse_chat_messages` from `parse.rs` for the collapsed summary view. For the expanded view, work directly with the raw `Value::Array` to show full content block details.

Add a new parsing function for the expanded detail:

```rust
/// Parsed message for the history explorer — richer than ChatMessage.
pub struct HistoryEntry {
    pub index: usize,
    pub role: String,
    /// Summary line for collapsed view.
    pub summary: String,
    /// Number of content blocks (for array content).
    pub block_count: usize,
    /// Total text length across all text blocks.
    pub text_len: usize,
    /// Content blocks for expanded view.
    pub blocks: Vec<HistoryBlock>,
    /// Flags for visual indicators.
    pub flags: EntryFlags,
}

pub struct HistoryBlock {
    pub block_type: String,       // "text", "tool_use", "tool_result"
    pub text: Option<String>,     // full text content
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub input_json: Option<String>, // pretty-printed tool input
}

pub struct EntryFlags {
    /// True if this message's text content is identical to another message.
    pub duplicate_content: bool,
    /// Index of the message this duplicates (if any).
    pub duplicate_of: Option<usize>,
}
```

### Duplicate Detection

After parsing all entries, scan for duplicates: compare text content of consecutive same-role messages. Flag entries where `text_len > 0` and the text matches a previous message of the same role. This makes the looping bug immediately visible — duplicated messages get a visual indicator.

## Rendering (history_view.rs)

### Collapsed View (default)

Each message is one or two lines:

```
  #0  user     "reverse the word hello"                    (1 block, 22 chars)
  #1  assistant "Sure! Let me use the reverse_text tool..." (2 blocks, 847 chars)
  #2  user      tool_result: reverse_text                  (1 block, 5 chars)
  #3  assistant "The reversed text is: olleh"              (1 block, 26 chars)
```

- Selected row gets a highlight/cursor indicator (`>` prefix or background color)
- Role gets a colored badge (same palette as thread view)
- Summary is first ~60 chars of combined text content, or `tool_use: {name}` / `tool_result: {name}` for tool blocks
- Metadata in parentheses: block count, total text chars
- Duplicate flag: a `[DUP of #N]` badge in a warning color when `flags.duplicate_content` is true

### Expanded View (after Enter on a message)

Expands inline below the summary line to show each content block:

```
> #1  assistant "Sure! Let me use the reverse_text..."     (2 blocks, 847 chars)
      [text] Sure! Let me use the reverse_text tool to reverse "hello" for you.
      [tool_use] reverse_text  id: toolu_01ABC...
        {"text": "hello"}
```

- Each block on its own line(s), indented
- Block type tag in brackets: `[text]`, `[tool_use]`, `[tool_result]`
- Text blocks show full content (wrapped to terminal width)
- Tool use blocks show name, truncated ID, and pretty-printed input JSON
- Tool result blocks show the result content

### Visual Indicators

- **Duplicate badge:** `[DUP of #N]` rendered in theme warning color next to the summary
- **Message count header:** Top line shows `History: {n} messages, {thread_id}` as a title
- **Live indicator:** When `turn.thinking` is true, show a pulsing `[streaming...]` indicator at the bottom

### Scrolling

Same scroll model as thread view — content rendered as a `Paragraph` with `Wrap` and `.scroll()` offset. Scrollbar on the right when content exceeds viewport. The selected row auto-scrolls into view when navigating with j/k.

## UiStore Changes (ui_store.rs)

Add `HistoryState` as a new `ActiveScreen` variant:

```rust
struct HistoryState {
    thread_id: String,
    selected_row: usize,
    scroll: usize,
    scroll_max: usize,
    viewport_height: usize,
    expanded: HashSet<usize>,
}
```

Handle `HistoryCommand` variants with the same patterns as `ThreadCommand` for scroll and `InboxCommand` for selection. `ToggleExpand` adds/removes the selected row from the `expanded` set.

Add `OpenHistory { thread_id }` to `GlobalCommand`. Add `BackToThread { thread_id }` to `GlobalCommand` for returning to the thread.

Update `UiSnapshot::editor()` to return `None` for `ScreenSnapshot::History` (no text editor on this screen).

## ox-cli Changes

### New Files

- `history_view.rs` — rendering function `draw_history()`, returns content height
- `history_shell.rs` — (optional, only if screen-specific key handling beyond InputStore is needed; likely not needed since all navigation goes through bindings)

### Modified Files

- `tui.rs` — add `ScreenSnapshot::History` arm to the content area match, calling `draw_history()`
- `view_state.rs` — add `ScreenSnapshot::History` arm to `fetch_view_state`, reading messages + turn state (same paths as thread)
- `bindings.rs` — add `h` binding on thread screen, add history screen bindings
- `event_loop.rs` — add `ScreenSnapshot::History` arm for scroll feedback (same pattern as thread), update `dispatch_key` to handle history screen

### ViewState Additions

```rust
pub struct ViewState<'a> {
    // ... existing fields ...
    /// Raw message values for the history explorer (only populated on history screen).
    pub raw_messages: Vec<Value>,
}
```

The history view needs the raw `Value::Array` elements (not just parsed `ChatMessage`s) to render expanded block details. `fetch_view_state` populates this only when on the history screen.

## Testing

- **parse.rs:** Add `parse_history_entries()` unit tests — basic messages, tool use/result, duplicate detection
- **ui_store.rs:** Test `HistoryCommand` handlers — selection bounds, expand/collapse toggle, scroll clamping
- **bindings.rs:** Test that `h` on thread screen is bound, history screen bindings exist

## Implementation Order

1. **ox-types:** Add `History` to Screen, ScreenSnapshot, UiCommand, GlobalCommand. Add HistorySnapshot, HistoryCommand structs.
2. **ox-ui/ui_store.rs:** Add HistoryState, handle HistoryCommand, handle OpenHistory/BackToThread transitions.
3. **ox-cli/parse.rs:** Add `HistoryEntry`, `HistoryBlock`, `EntryFlags`, `parse_history_entries()` with duplicate detection.
4. **ox-cli/bindings.rs:** Add `h` on thread screen, all history screen bindings.
5. **ox-cli/view_state.rs:** Add history arm to `fetch_view_state`, populate `raw_messages`.
6. **ox-cli/history_view.rs:** New file — `draw_history()` with collapsed/expanded rendering.
7. **ox-cli/tui.rs:** Wire history screen into the draw function.
8. **ox-cli/event_loop.rs:** Wire history screen into scroll feedback and key dispatch.
