# Synthesis and Complete Tool Design

## Problem

The current system has a magic `read("prompt")` path that hides synthesis
behind a hardcoded function. `synthesize_prompt()` reads four fixed paths,
produces a `CompletionRequest` struct (coupled to one provider's wire shape),
and returns it. Structured context (file spans, pins, lazy providers, history
views) has no path into this. Multi-completion turns are impossible because
the kernel's three-phase API assumes one completion per turn.

## Design

### The `complete` tool

`complete` is an LLM-facing tool — it appears in `tools/schemas`, the LLM
can call it. It's also what the kernel uses for bootstrap. One tool, two
callers.

**Input:** An account name and an array of typed context references.

```json
{
    "account": "anthropic",
    "refs": [
        {"type": "system", "path": "system"},
        {"type": "history", "path": "history/messages", "last": 20},
        {"type": "tools", "path": "tools/schemas", "only": ["read_file", "shell"]},
        {"type": "span", "file": "src/auth.rs", "lines": [3, 45]},
        {"type": "pin", "id": "auth-context"}
    ]
}
```

**What it does internally:**

1. **Resolve refs** via its reader handle — path refs read from the
   namespace, span refs read from disk, pin refs look up stored context.
   Refs that fail to resolve produce warnings, not fatal errors.
2. **Assemble for target** — the provider config for the named account
   determines wire format (Anthropic, OpenAI, etc.), token budget,
   model capabilities. Synthesis counts tokens using the target's
   tokenizer and truncates if the budget is exceeded (history windowed
   first, then spans dropped, system prompt never truncated).
3. **Send** — wire formatting + HTTP transport. Streams events to
   `events/emit` for real-time TUI delivery as a side effect.
4. **Inner loop** — if the LLM response contains tool calls, the
   `complete` tool executes them (via the normal tool execution path),
   records results to the structured log, and fires another completion.
   Repeats until the inner loop resolves (no more tool calls). The
   final text response is the tool result.

**What it returns:** A handle. Reading the handle blocks until the
completion (and any inner tool-call loops) resolve, then returns the
final response Value.

### Reader handle

The `complete` tool owns an independent reader handle to resolve refs
without being tangled in the namespace's borrow chain. Wired at
construction:

- CLI: broker client scoped to the thread
- Web: a separate reader adapter

Same pattern as GateStore's config handle and CliCompletionTransport's
scoped client.

### Inner completion context

When the LLM calls `complete` as a tool, the inner completion sees ONLY
what the refs specify. The outer agent's full history is not inherited.
The LLM explicitly controls what each sub-completion sees via its refs.

The inner loop writes to the same structured log (auditability), but
entries are tagged with a scope ID. The outer agent's history view
filters out inner-loop entries.

### Kernel bootstrap

The kernel (Wasm pico-process) handles the first completion. This is the
one place the kernel has hardcoded knowledge of how to call the
`complete` tool.

```
// Bootstrap: kernel constructs refs from what it knows
let refs = [
    {type: "system", path: "system"},
    {type: "history", path: "history/messages"},
    {type: "tools", path: "tools/schemas"},
]
let config = read("gate/defaults")

let h = write("tools/complete", {account: config.account, refs: refs})
let response = read(h)
```

After bootstrap, the LLM is in the driver's seat. The kernel's loop
is generic: execute tool calls from responses, record to log, loop
until no more tool calls.

### Multi-completion

The LLM calls `complete` multiple times in one response. Each call
specifies different refs (different context, different account). The
kernel executes them as handles, awaits them in parallel, returns
results. The LLM synthesizes the results.

```
LLM response tool calls:
  complete({account: "fast", refs: [system, history.last(5)]})
  complete({account: "strong", refs: [system, history, all_spans]})
  read_file({path: "src/auth.rs"})

Kernel: three handles, awaited concurrently, results back to LLM.
```

This is not a framework feature. It's the LLM deciding to use the
`complete` tool multiple times. The framework just executes tool calls.

### Structured log and history views

**Log** (`log/`) — append-only, everything recorded. Every LLM response,
every tool call and result, every inner-loop round-trip. Tagged with
metadata: source account/model, scope ID (for inner completions),
timestamps, token usage.

Log entries:
- `{type: "user", content: "..."}`
- `{type: "assistant", content: [...], source: {account, model}}`
- `{type: "tool_call", id, name, input}`
- `{type: "tool_result", id, output}`
- `{type: "meta", ...}` — synthesis params, timing, tokens

**History views** (`history/`) — read-only projections over the log.
Pluggable history provider reads the log and produces filtered views:

- `history/messages` — default view. Collapses inner-loop chatter,
  respects windowing, may summarize old turns.
- `history/messages/last/N` — last N messages.
- `history/full` — unfiltered.

The history provider is pluggable. Mount a different one, get different
filtering. Simple pass-through for development, token-aware windowing
for production, RAG-augmented for retrieval-heavy use cases.

### What disappears

| Current | Replacement |
|---------|-------------|
| `synthesize_prompt()` in ox-context | Synthesis step inside the `complete` tool |
| `read("prompt")` intercept in Namespace and HostStore | No magic paths — kernel calls `complete` tool |
| `CompletionRequest` type at kernel boundary | Internal detail of the `complete` tool |
| `Kernel::initiate_completion` / `consume_events` / `complete_turn` | Kernel calls `complete` as a tool, executes results |
| `TurnStore` | Handles are the state |
| `KernelState` enum | Position derived from the log |

### What's new

| Component | Responsibility |
|-----------|---------------|
| `complete` tool | Synthesis (ref resolution + API formatting) + transport (HTTP) + inner loop |
| Structured log (`log/`) | Append-only audit trail, tagged entries |
| History provider (`history/`) | Read-only views over the log, pluggable filtering |
| Handle registry | Execution handles for async tool/completion resolution |

### Reference type vocabulary

The `complete` tool's schema publishes what reference types it supports.
The LLM reads the schema and reasons about which refs to include. The
kernel's bootstrap hardcodes the basic set (system, history, tools).

Initial reference types:

| Type | Params | Resolution |
|------|--------|------------|
| `system` | `path` | Read string from path |
| `history` | `path`, `last` (optional) | Read messages, optionally window |
| `tools` | `path`, `only` (optional), `except` (optional) | Read schemas, optionally filter |
| `span` | `file`, `lines` (optional) | Read file from disk, extract line range |
| `pin` | `id` | Look up pinned context entry by ID |
| `raw` | `content` | Literal string, included as-is |

The schema is extensible — new reference types can be added to the
`complete` tool without changing the kernel.

### Separation from fork

`complete` is a sub-routine — same agent, nested execution, synchronous
from the caller's perspective. The inner loop runs to resolution and
returns the final response as a tool result.

Fork creates a new agent with derived context and independent execution.
The parent gets a handle and moves on. The child has its own lifecycle
and its own structured log. Fork is built on different primitives than
`complete`.
