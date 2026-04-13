# Tool Result Abbreviation & Retrieval

**Date:** 2026-04-12
**Status:** Draft
**Depends on:** Agent loop fixes (system prompt, nudge, iteration cap — already landed)

## Problem

Large tool outputs (e.g. `cargo test` producing 500 lines, `cat` of a big file) bloat the
conversation history. Every subsequent LLM completion sees the full output in its context,
which:

1. Wastes tokens on content the model has already processed
2. Can cause the model to lose track of its task and loop
3. Fills the context window, crowding out earlier conversation that may be more relevant

## Design Principles

- **Full results are always stored** — the SharedLog is the source of truth, never truncated
- **Abbreviation is a projection concern** — the history view controls what the model sees
- **Retrieval is a tool** — the model requests full/partial results through normal tool dispatch
- **Source-side control via tool parameters** — the model can proactively limit output
- **No kernel special cases** — all tools dispatch through the ToolStore, no `if name == "..."` in the kernel

## Architecture

```
SharedLog (Arc<Mutex<Vec<LogEntry>>>)
  ├── LogStore        — append-only structured log            (mounted at log/)
  ├── HistoryView     — wire-format projection, abbreviates   (mounted at history/)
  └── ResultViewer    — random-access by tool_use_id          (new module in ox-tools)
```

Three views over the same data. Same pattern as LogStore + HistoryView today.

## Components

### 1. History abbreviation (ox-history)

**Where:** `HistoryView::project_messages()` in `crates/ox-history/src/lib.rs`

**What:** When projecting a `LogEntry::ToolResult` into a wire-format tool_result message,
check the line count. If it exceeds a threshold, show head + tail lines with an omission
marker.

**Constants:**
- `ABBREVIATE_THRESHOLD_LINES = 50` — results under this are shown in full
- `ABBREVIATE_HEAD_LINES = 20` — lines kept from the top
- `ABBREVIATE_TAIL_LINES = 20` — lines kept from the bottom

**Omission marker format:**
```
[... {N} lines omitted — use get_tool_output with tool_use_id="{id}" to see full output,
 or re-run the command with max_lines to limit output at the source]
```

The marker references the `tool_use_id` so the model knows exactly how to retrieve the
full output. It also nudges toward `max_lines` for proactive control.

**Edge cases:**
- Non-string outputs (JSON objects) — serialize to string, then abbreviate
- Empty results — pass through unchanged
- Results with fewer than threshold lines — pass through unchanged
- Binary/non-UTF8 content — shouldn't reach here (executor returns strings), but handle gracefully

### 2. Result retrieval tool — ResultViewer (ox-tools)

**New file:** `crates/ox-tools/src/result_viewer.rs`

**What:** A tool module that holds a `SharedLog` clone and provides random-access
retrieval of tool results by `tool_use_id`.

**SharedLog dependency:** `ResultViewer::new(shared: SharedLog)` — same pattern as
`HistoryView::new(shared)` and `LogStore::from_shared(shared)`. The SharedLog is
`Clone` (it's an `Arc<Mutex<...>>`), so this is free.

**SharedLog method needed:** `SharedLog::tool_result_output(&self, tool_use_id: &str) -> Option<serde_json::Value>` — searches the log (from the end, since recent results are most common) for a matching `LogEntry::ToolResult`.

**Tool schema:**
```json
{
  "name": "get_tool_output",
  "description": "Retrieve the full or partial output of a previous tool call. Use this when a tool result was abbreviated in the conversation. Specify offset and limit to retrieve specific line ranges.",
  "input_schema": {
    "type": "object",
    "properties": {
      "tool_use_id": {
        "type": "string",
        "description": "The tool_use_id from the abbreviated result"
      },
      "offset": {
        "type": "integer",
        "description": "0-based line offset to start from (default: 0 = beginning)"
      },
      "limit": {
        "type": "integer",
        "description": "Maximum number of lines to return (default: all remaining lines)"
      }
    },
    "required": ["tool_use_id"]
  }
}
```

**Execution:** `ResultViewer::execute(tool_use_id, offset?, limit?)`:
1. Look up result in SharedLog by tool_use_id
2. Convert output to string
3. If offset/limit provided, slice by lines
4. Return the result string (with line range header if sliced)

**Registration:** The ToolStore constructor takes a `ResultViewer` (or it's added via
a setter, since the SharedLog isn't available until thread namespace construction).

**Wiring:** In `ThreadNamespace::new_default()` (thread_registry.rs), the SharedLog
is created and shared with HistoryView, LogStore, and now ResultViewer. The ResultViewer
is passed to the ToolStore (or registered after construction).

### 3. Source-side output control — max_lines on shell (ox-tools)

**Where:** `OsModule` in `crates/ox-tools/src/os.rs`

**What:** Add optional `max_lines` parameter to the `shell` tool schema. When provided,
the executor truncates stdout to that many lines before returning.

**Schema change:**
```json
{
  "properties": {
    "command": { "type": "string", "description": "Shell command to execute" },
    "max_lines": {
      "type": "integer",
      "description": "Maximum lines of output to return. Use this for commands that may produce large output. Omit for full output."
    }
  },
  "required": ["command"]
}
```

**Implementation:** The `max_lines` parameter is passed through to the executor binary
via the `ExecCommand.args`. The executor (ox-tool-exec) handles truncation of the
stdout result, appending a `[... truncated at {max_lines} lines, {total} total]` marker.

**Why at the executor, not in OsModule?** The executor already owns the subprocess
stdout. Truncating there avoids reading the full output into memory just to discard it.

### 4. Optional: max_lines on fs_read

Same pattern as shell. The `fs_read` tool gets optional `offset` (line) and `limit` (lines)
parameters. The executor returns the requested range with a header indicating position.

This is lower priority — file reads are usually targeted. But it follows the same pattern
and gives the model the same kind of control.

## Wiring Summary

| Component | Crate | Change Type |
|-----------|-------|-------------|
| `SharedLog::tool_result_output()` | ox-kernel | New method on existing type |
| `abbreviate_tool_result()` | ox-history | New function, called from `project_messages()` |
| `ResultViewer` | ox-tools | New module |
| `ToolStore` construction | ox-tools | Accept/register `ResultViewer` |
| `ThreadNamespace::new_default()` | ox-cli | Pass `SharedLog` clone to `ResultViewer` |
| `shell` schema + executor | ox-tools + ox-tool-exec | Add `max_lines` parameter |
| Tool schema writes | ox-cli (agents.rs) | No change — schemas auto-aggregate |

## What Does NOT Change

- **ox-kernel/run.rs** — `execute_tools` stays generic, no tool-name special cases
- **Context synthesis** — `ContextRef::History` reads from `history/messages` as before
- **LogStore** — Stores full results, no truncation
- **SharedLog** — Append-only, never mutated after write

## Priority

1. **History abbreviation + SharedLog lookup** — Immediately reduces context bloat
2. **ResultViewer tool** — Gives the model retrieval capability
3. **max_lines on shell** — Source-side control for proactive models
4. **max_lines on fs_read** — Nice-to-have, same pattern
