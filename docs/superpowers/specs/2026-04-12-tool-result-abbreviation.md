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

- **Full results are always stored** — the LogStore is the source of truth, never truncated
- **Abbreviation is a projection concern** — the history view controls what the model sees
- **Retrieval via StructFS paths** — the model reads from `log/results/{id}` through normal routing
- **Source-side control via tool parameters** — the model can proactively limit output
- **No kernel special cases** — `execute_tools` stays generic
- **Redirect tools** — a tool whose "execution" returns a namespace path, not a computed result

## Architecture

```
LogStore (mounted at log/)
  ├── log/entries                                  — all log entries
  ├── log/count                                    — entry count
  ├── log/last/{n}                                 — last n entries
  ├── log/results/{tool_use_id}                    — full result string for a tool call
  ├── log/results/{tool_use_id}/line_count         — number of lines in result
  └── log/results/{tool_use_id}/lines/{offset}/{limit}  — line-ranged slice

HistoryView (mounted at history/)
  └── history/messages                             — wire-format projection, abbreviates large results

ToolStore (mounted at tools/)
  └── get_tool_output → redirect to log/results/…  — LLM-facing tool, dispatches via redirect
```

LogStore is the single source of truth. HistoryView abbreviates when projecting.
The `get_tool_output` tool is a **redirect** — its execution builds a `log/results/…`
path from the input parameters, and the kernel reads the result from LogStore through
normal namespace routing.

## Components

### 1. LogStore result read paths (ox-kernel)

**Where:** `LogStore` reader in `crates/ox-kernel/src/log.rs`

**New read paths on LogStore:**

```
results/{tool_use_id}                    → Value::String(full output)
results/{tool_use_id}/line_count         → Value::Integer(n)
results/{tool_use_id}/lines/{offset}/{limit} → Value::String(sliced output with header)
```

**Implementation:** `LogStore::read` matches on the `"results"` prefix. Scans
`SharedLog` entries (from the end) for a `LogEntry::ToolResult` with matching `id`.
Converts `output` to string, applies line slicing if sub-path present.

**Line-ranged output format:**
```
[lines {start+1}-{end} of {total}]
{sliced content}
```

**Edge cases:**
- Unknown tool_use_id → `StoreError` (not found)
- Offset beyond end → empty result with header `[lines {total+1}-{total} of {total}]`
- No limit sub-path → full result string

### 2. History abbreviation (ox-history)

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

The marker references the `tool_use_id` so the model knows how to retrieve the full
output. It also nudges toward `max_lines` for proactive source-side control.

**Edge cases:**
- Non-string outputs (JSON objects) — serialize to string, then abbreviate
- Empty results — pass through unchanged
- Results with fewer than threshold lines — pass through unchanged

### 3. Redirect tool dispatch (ox-tools + ox-runtime)

**Concept:** A **redirect tool** is a tool whose execution does not compute a result.
Instead, it returns a **namespace path** as the exec handle. The kernel reads the
result from that path through normal namespace routing.

Today all exec handles are ToolStore-internal (`exec/0001`). The HostStore prepends
`tools/` before returning the handle to the kernel. For redirect tools, the ToolStore
returns a namespace-absolute path that the HostStore passes through without re-prefixing.

**Convention:** If the path returned by ToolStore starts with a recognized namespace
root (anything other than `exec/`), HostStore treats it as namespace-absolute and
does not prepend `tools/`.

**HostStore change** (in `handle_write`):
```rust
let result_path = self.effects.tool_store().write(&sub, data)?;
if result_path.components.first().map(|c| c.as_str()) == Some("exec") {
    // ToolStore-internal handle — prefix with tools/
    let mut components = vec!["tools".to_string()];
    components.extend(result_path.components);
    Ok(Path::from_components(components))
} else {
    // Namespace-absolute redirect — pass through
    Ok(result_path)
}
```

### 4. get_tool_output tool (ox-tools)

**Wire name:** `get_tool_output`
**Internal routing:** redirect to `log/results/{tool_use_id}[/lines/{offset}/{limit}]`

**Tool schema:**
```json
{
  "name": "get_tool_output",
  "description": "Retrieve the full or partial output of a previous tool call. Use this when a tool result was abbreviated in the conversation.",
  "input_schema": {
    "type": "object",
    "properties": {
      "tool_use_id": {
        "type": "string",
        "description": "The tool_use_id from the abbreviated result"
      },
      "offset": {
        "type": "integer",
        "description": "0-based line offset to start from (default: 0)"
      },
      "limit": {
        "type": "integer",
        "description": "Maximum number of lines to return (default: all)"
      }
    },
    "required": ["tool_use_id"]
  }
}
```

**Implementation:** A new module or method on ToolStore that:
1. Extracts `tool_use_id`, optional `offset`, optional `limit` from input
2. Builds a namespace path: `log/results/{tool_use_id}` or `log/results/{tool_use_id}/lines/{offset}/{limit}`
3. Returns the path (not `exec/NNNN`) — HostStore recognizes this as a redirect

The ToolStore does NOT read from the log. It builds a path. The kernel reads the path
through the namespace. LogStore serves the data. Clean separation.

### 5. Source-side output control — max_lines on shell (ox-tools)

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
via `ExecCommand.args`. The executor truncates stdout, appending
`[... truncated at {max_lines} lines, {total} total]`.

**Why at the executor?** The executor owns the subprocess stdout. Truncating there
avoids buffering the full output just to discard most of it.

### 6. Optional: offset/limit on fs_read

Same pattern. The `fs_read` tool gets optional `offset` (line) and `limit` (lines)
parameters for line-ranged file reads.

Lower priority — file reads are usually targeted. Same pattern, same kind of control.

## Wiring Summary

| Component | Crate | Change Type |
|-----------|-------|-------------|
| `LogStore` read: `results/{id}`, `/line_count`, `/lines/{o}/{l}` | ox-kernel | New read paths |
| `abbreviate_tool_result()` | ox-history | New function, called from `project_messages()` |
| Redirect tool concept | ox-tools | New dispatch path in ToolStore write |
| HostStore redirect detection | ox-runtime | Small change in `handle_write` |
| `get_tool_output` tool + schema | ox-tools | New redirect tool registration |
| `shell` schema + executor | ox-tools + ox-tool-exec | `max_lines` parameter |
| Thread namespace wiring | ox-cli | No change — LogStore already mounted, schemas auto-aggregate |

## What Does NOT Change

- **ox-kernel/run.rs** — `execute_tools` stays fully generic
- **Context synthesis** — `ContextRef::History` reads from `history/messages` as before
- **SharedLog** — Append-only, never mutated after write. No new consumers of raw SharedLog
- **LogStore as source of truth** — Results are written to and read from LogStore. No bypass

## Priority

1. **LogStore result paths + history abbreviation** — Immediately reduces context bloat
2. **Redirect tool concept + get_tool_output** — Gives the model retrieval via StructFS
3. **max_lines on shell** — Source-side control for proactive models
4. **offset/limit on fs_read** — Nice-to-have, same pattern
