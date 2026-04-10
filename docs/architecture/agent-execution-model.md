# Agent Execution Model

## Core Abstraction

An **agent** is a portable context bundle. A **kernel** is the executor that
runs it.

The agent is data — a StructFS namespace containing everything the agent knows
and is doing: system prompt, conversation history, tool configuration, model
accounts, and in-flight execution handles. This context bundle is serializable,
resumable, and portable. Ship it to a different host and any kernel can pick it
up and keep going.

The kernel is the runtime. It reads the context to determine what to do next,
fires effects (completions, tool calls) as writes that return handles, reads
handles for results, and writes results back to the context. The kernel holds
no state of its own — all state lives in the context it's executing.

```
┌─────────────────────────────────────────────────────────┐
│                    Agent (context bundle)                │
│                                                         │
│  system/     history/     tools/     gate/    context/   │
│  ┌───────┐   ┌───────┐   ┌──────┐   ┌─────┐ ┌────────┐ │
│  │prompt │   │messages│   │schemas│  │accts│ │spans   │ │
│  │       │   │       │   │exec/* │   │model│ │pins    │ │
│  └───────┘   └───────┘   └──────┘   └─────┘ │refs    │ │
│                                              └────────┘ │
└─────────────────────────────────────────────────────────┘
        ▲                                    │
        │ reads context                      │ writes effects
        │ writes results                     │ reads handles
        │                                    ▼
┌─────────────────────────────────────────────────────────┐
│                    Kernel (executor)                     │
│                                                         │
│  Runs inside the Wasm pico-process.                     │
│  Stateless — all state is in the context above.         │
│  Same binary everywhere. Context is what varies.        │
│                                                         │
└─────────────────────────────────────────────────────────┘
        ▲                                    │
        │ handle resolution                  │ syscalls
        │ (results flow up)                  │ (effects flow down)
        ▼                                    ▼
┌─────────────────────────────────────────────────────────┐
│                    Host (provides the namespace)         │
│                                                         │
│  Resolves handles: spawns subprocesses, HTTP calls,     │
│  in-process functions. Applies policy (PolicyStore).    │
│  Applies sandbox (ClashSandboxPolicy). Emits events     │
│  to TUI. Persists context to disk.                      │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

The relationship between these three layers is the same as process / CPU /
kernel in an operating system:

| OS concept | Ox concept | Role |
|------------|-----------|------|
| Program (data in memory) | Agent (context bundle) | The thing being executed |
| CPU | Kernel | The executor — reads instructions, produces effects |
| OS kernel | Host | Provides syscalls, enforces policy, manages resources |

## Handle-Based Execution

Every effect the kernel produces is a StructFS write that returns a handle.
Every result the kernel consumes is a StructFS read that resolves a handle.
This is uniform across all effect types.

```
// Completion — same pattern as a tool call
c1 = write("tools/completions/complete/anthropic", prompt_value_1)
c2 = write("tools/completions/complete/openai", prompt_value_2)

// Tool calls — same pattern as a completion
t1 = write("tools/read_file", {path: "src/lib.rs"})
t2 = write("tools/shell", {command: "cargo test"})

// Await any set of handles — completions and tools mixed freely
batch = write("tools/await", [c1, c2, t1, t2])
results = read(batch)
```

Writes return handles immediately. The host executes effects asynchronously
(subprocesses, HTTP, in-process). Reads block until the handle resolves.
`tools/await` accepts any set of handles and returns a composite handle whose
read blocks until all constituents resolve.

The kernel does not know whether a handle wraps a subprocess, an HTTP call, or
an in-process function. It does not know whether execution is local or remote.
It writes, gets a handle, reads the handle. That's it.

### The Prompt Is a Value

There is no `CompletionRequest` type at the kernel boundary. The kernel
deals in `Value` — StructFS's native data type. The kernel reads Values
from context, writes Values to completion paths. The host translates
Values to provider wire formats (Anthropic JSON, OpenAI JSON, etc.).

`read("prompt")` is a **convenience provider** that assembles a
completion-ready Value from the context stores (system, history, tools,
model config). The kernel can use it for the common single-completion case.
But the kernel is equally free to read context components directly and
construct its own Values:

```
let system = read("system")
let history = read("history/messages")
let tools = read("tools/schemas")

// Construct a focused prompt with a subset of tools and recent history
let focused_value = assemble(system, history.last(10), tools.subset(["read_file"]))
let h = write("tools/completions/complete/anthropic", focused_value)
```

The `prompt` path is sugar, not architecture. The architecture is: read
Values from context, write Values to effect paths, read handles for
results. All Values. No special types crossing the boundary.

---

## Life of an Agent

### 1. Birth: Context Creation

An agent begins as an empty context bundle. The host creates a namespace
and mounts the initial providers:

```
namespace = Namespace::new()
namespace.mount("system",  SystemProvider("You are a coding assistant."))
namespace.mount("history", HistoryProvider::new())
namespace.mount("tools",   ToolStore::new(fs, os, completions))
namespace.mount("gate",    GateStore::new().with_config(config))
namespace.mount("context", ContextManager::new())  // pluggable context providers
```

At this point the agent exists but has never run. Its context contains a
system prompt, empty history, tool schemas, and model configuration. No
messages, no state, no handles.

### 2. Bootstrap: First Input

The host writes the user's first message to the context:

```
write("history/append", {role: "user", content: "Fix the bug in auth.rs"})
```

Then the host starts the kernel. On the CLI, this means loading the Wasm
module and calling its `run()` export. The kernel begins executing inside
the Wasm pico-process, with the namespace as its filesystem.

### 3. Startup: Reading Context

The kernel's first act is reading its context to determine where it is.
This is the same code path for fresh starts and crash recovery.

```
// Read history to determine state
let messages = read("history/messages")
let last = messages.last()

match last.role {
    "user" => {
        // Normal case: user message waiting for response.
        // Proceed to completion.
    }
    "assistant" if has_tool_use_blocks(last) => {
        // Crash recovery: assistant requested tools but no results recorded.
        // Re-issue the tool calls before entering the main loop.
        let pending = extract_tool_calls(last)
        execute_and_record(pending)
    }
    "assistant" => {
        // Turn already complete. Nothing to do.
        return
    }
    "tool_result" => {
        // Tools completed but next completion never fired.
        // Proceed to completion (tools results are in history).
    }
}
```

The kernel derives its position entirely from context. There is no saved
instruction pointer, no phase enum, no "where was I" state. The context
IS the state.

### 4. The Loop: Single-Completion Turn

The simplest case: one completion, one model, standard prompt assembly.

```
loop {
    // Read the assembled prompt — a Value, not a CompletionRequest
    let prompt = read("prompt")

    // Write to the completion path — returns a handle immediately
    let h = write("tools/completions/complete/anthropic", prompt)

    // Read the handle — blocks until the completion finishes streaming
    let response = read(h)

    // Process the response into content blocks (text, tool_use)
    let content = accumulate_response(response)

    // Write the assistant message to history
    write("history/append", assistant_message(content))

    // Extract tool calls from the response
    let tool_calls = extract_tool_calls(content)

    if tool_calls.is_empty() {
        // Model responded with text only. Turn is done.
        return
    }

    // Fire tool calls, await results, record to history
    execute_and_record(tool_calls)

    // Loop: the tool results are now in history, so the next
    // read("prompt") will include them, and the model sees the results.
}
```

Each iteration is: read context → fire completion → process response →
maybe fire tools → record everything → loop. The context grows with each
iteration. The model sees the full conversation on each completion.

### 5. Tool Execution: Handles In Practice

When the model requests tool calls, the kernel fires them all and awaits
the results:

```
fn execute_and_record(tool_calls) {
    // Fire all tool writes — each returns a handle immediately
    let handles = []
    for tc in tool_calls {
        write("events/emit", {type: "tool_call_start", name: tc.name})
        let h = write("tools/{tc.name}", tc.input)
        handles.push({id: tc.id, name: tc.name, handle: h})
    }

    // Write all handles to await — blocks until every tool completes
    let handle_ids = handles.map(|h| h.handle)
    let batch = write("tools/await", handle_ids)
    let results = read(batch)

    // Emit completion events and record to history
    for (handle, result) in zip(handles, results) {
        write("events/emit", {type: "tool_call_result", name: handle.name, ...})
    }
    write("history/append", tool_results_message(handles, results))
}
```

The host resolves handles concurrently. Three tool calls that each take
1 second complete in ~1 second total, not 3. The kernel doesn't manage
concurrency — it fires writes and awaits handles. The host manages the
parallelism.

Behind each handle, the host applies policy (`PolicyCheck` — should this
tool run?) and sandbox (`ClashSandboxPolicy` — what can the subprocess
access?). The kernel sees none of this. A denied tool returns an error
Value through the handle, same as a tool that failed.

### 6. Multi-Completion Turn: The General Case

The single-completion turn is the degenerate case. The general case:
the kernel reads its context, decides it needs input from multiple
models, constructs multiple prompt Values, and fires them all.

**Example: Consensus coding.** The kernel wants three models to review
a code change before committing:

```
// Read context components directly — not the pre-assembled prompt
let system = read("system")
let history = read("history/messages")
let tools = read("tools/schemas")

// Construct a review-focused prompt Value
let review_prompt = {
    system: system + "\nYou are reviewing code for correctness.",
    messages: history,
    tools: [],  // no tools for review — just analysis
    model: "claude-sonnet-4-20250514",
    max_tokens: 4096,
}

// Fire to three models
let c1 = write("tools/completions/complete/anthropic", review_prompt)
let c2 = write("tools/completions/complete/openai",
               review_prompt.with(model: "gpt-4o"))
let c3 = write("tools/completions/complete/anthropic",
               review_prompt.with(model: "claude-haiku-4-5-20251001"))

// Await all three
let batch = write("tools/await", [c1, c2, c3])
let responses = read(batch)

// Synthesize: all three agree? Proceed. Disagreement? Investigate.
let consensus = analyze_responses(responses)
```

**Example: Parallel sub-tasks.** The kernel decomposes a task and farms
pieces to different models simultaneously:

```
let c1 = write("tools/completions/complete/anthropic", {
    system: "Write unit tests for auth.rs",
    messages: [file_span("src/auth.rs")],
    ...
})
let c2 = write("tools/completions/complete/anthropic", {
    system: "Write integration tests for auth.rs",
    messages: [file_span("src/auth.rs"), file_span("tests/helpers.rs")],
    ...
})

let batch = write("tools/await", [c1, c2])
let [unit_tests, integration_tests] = read(batch)
```

The kernel constructs the prompt Values. The kernel decides how many
completions to fire. The kernel decides which models to use. The host
just resolves handles. This is the kernel's intelligence — not a
framework feature, but the agent's own reasoning expressed as
StructFS writes.

### 7. Context Management Within the Loop

The kernel doesn't just consume context — it actively manages it.
Context providers are surfaced as tools, and the kernel manipulates
them within the loop:

```
// The kernel reads a file, decides it's relevant, pins it
let file = read_result_from_tool("tools/read_file", {path: "src/auth.rs"})
write("context/pin", {
    id: "auth-source",
    content: file,
    label: "auth.rs source"
})
// Future reads of "prompt" now include this pinned span

// Later, the kernel decides this context is no longer needed
write("context/drop", {id: "auth-source"})

// The kernel can window history to manage prompt size
write("context/history/window", {last_n: 20})
```

Every `read("prompt")` (or direct component read) produces a *view* —
assembled on demand from whatever providers are currently mounted and
configured. The kernel shapes its own view by writing to context
management paths. This is loop resolution: the kernel tunes its own
prompt as part of its reasoning process.

### 8. Fork: Spawning Sub-Agents

When the kernel needs to delegate work, it forks. Fork is a
context-to-context transform — the child inherits pieces of the
parent's context cheaply without re-synthesis.

```
// Parent kernel forks a sub-agent for a focused task
let child_handle = write("tools/fork", {
    // Transform spec: what the child inherits
    system: "You are a test writer. Write tests for the given code.",
    inherit: {
        context: ["auth-source"],  // inherit pinned spans by ID
        tools: ["read_file", "write_file", "shell"],  // subset
        gate: ["anthropic"],  // subset of accounts
    },
    task: "Write comprehensive tests for src/auth.rs",
})

// The child runs with its own kernel, its own context, its own loop.
// The parent continues — child execution is a handle like anything else.

// Later, read the child's result
let child_result = read(child_handle)
```

The child's context is derived from the parent's context by the
transform. File spans are shared references, not copies — the child
sees the same data without re-reading files or re-prompting an LLM.
Tool schemas are filtered, not regenerated. The system prompt is
composed, not re-synthesized.

The child is a full agent with its own kernel. It can fire its own
completions, use its own tools, manage its own context. Its results
flow back to the parent through the handle.

### 9. Quiescence: Turn Complete

The kernel exits when it has nothing left to do — the model responded
with text only (no tool calls) and the kernel has no further work.

```
// Model responded with text, no tool calls
let tool_calls = extract_tool_calls(content)
if tool_calls.is_empty() {
    // Write turn-end event for observability
    write("events/emit", {type: "turn_end"})
    return
}
```

The kernel's `run()` function returns. The Wasm pico-process exits.
The host reads the context — history now contains the full conversation
including the model's final response. The context is the result.

The host persists the context to disk (snapshot). The agent is now
suspended data. It can be:
- **Resumed** — user sends another message, host appends to history,
  starts a new kernel with the same context.
- **Shipped** — context serialized and sent to a remote host.
- **Forked** — another agent inherits this context as a starting point.
- **Inspected** — read the namespace to see everything that happened.

### 10. Resumption: Next User Message

When the user sends another message, the host appends it to history and
starts a new kernel with the existing context:

```
// Host side
write("history/append", {role: "user", content: "Now optimize it"})
kernel.run(context)  // new kernel, same context
```

The kernel reads history, sees the new user message at the end, and
proceeds with a new completion. The full conversation history is
available to the model. The kernel doesn't know this is the second
turn or the hundredth — it reads context and acts.

### 11. Crash Recovery in Detail

If the process dies mid-execution (host crash, OOM, power loss), the
context on disk reflects the last successful persistence point. On
restart:

```
// Host restores context from disk
let context = restore_from_snapshot(thread_dir)

// Host appends no new message — just starts the kernel
kernel.run(context)
```

The kernel reads history and detects the crash:

**Case A: Crash during completion.** Last message is "user" — the
completion never started or never completed. The kernel proceeds
normally (fires a new completion). No data loss — the completion
wasn't recorded yet.

**Case B: Crash after completion, before tools.** Last message is
"assistant" with tool_use blocks. No tool results follow. The kernel
re-issues all tool calls. Idempotency is the tool's responsibility.

**Case C: Crash during tool execution.** If individual handle results
are persisted to context as they complete (not batched), the kernel
can detect which tools finished and which didn't. It re-issues only
the incomplete ones. If results are not persisted mid-batch, this
degrades to Case B.

**Case D: Crash after tools, before next completion.** Last message is
"tool_result". The kernel reads the prompt (which includes tool
results) and fires the next completion. No re-execution needed.

The kernel never needs to know it crashed. It reads context, acts on
what it finds. The startup path and the recovery path are the same code.

---

## Context Is a View, Not a Bag of Data

Context is not "the stuff that goes in the prompt." Context is a pluggable
system of providers that synthesize prompt content on demand. History is one
provider. But context also includes:

- **File spans** — regions of source files, lazily loaded, included when
  relevant. Not the whole file, not a summary — the exact span.
- **Command output** — results of executed commands, captured and mounted
  as context providers.
- **Lazy computation** — values computed on read, not stored. A provider
  that reads the filesystem, queries a database, or calls an API when
  the kernel reads `prompt`.
- **Structured references** — pointers to external resources (URLs,
  database rows, API endpoints) that resolve on demand.

Even history is not literally "in context." The prompt synthesis reads
`history/messages`, but what it gets back is a *view* over history —
potentially truncated, summarized, filtered, or windowed by the history
provider. The provider decides what history looks like in the prompt.
Different providers produce different views of the same underlying data.

### Context as Tool Surface

Context providers are surfaced to the kernel as tools. The kernel can
manipulate its own context within the loop:

- **Pin a file span** — `write("context/pin", {path, start, end})` adds
  a span that stays in the prompt across turns.
- **Drop a reference** — `write("context/drop", {ref_id})` removes a
  context entry that's no longer needed.
- **Summarize** — `write("context/summarize", {ref_id})` replaces a
  verbose entry with a compressed form.
- **Window history** — `write("context/history/window", {last_n})` adjusts
  how much history appears in the prompt.

The kernel is an active participant in context management, not a passive
consumer. It reads its context, decides what's relevant, reshapes it for
the next completion. This is loop resolution — the kernel tunes its own
prompt within the agentic loop.

### Fork: Context-to-Context Transform

When the kernel spawns a sub-agent (fork), it produces a *transform* from
its context to the sub-agent's context. The sub-agent inherits pieces of
the parent's context cheaply — file spans, tool schemas, system prompt
fragments — without re-synthesizing them through an LLM.

```
parent_context ──transform──> child_context
     │                            │
     │ file spans: inherited      │ file spans: same refs
     │ history: filtered/none     │ history: empty or subset
     │ tools: subset              │ tools: scoped subset
     │ system: parent + task      │ system: derived prompt
     │ gate: parent accounts      │ gate: subset of accounts
```

The transform is a function `context -> context` that:
- Selects which providers to inherit (shallow copy, not re-synthesis)
- Filters or scopes inherited providers (e.g., subset of tools)
- Adds task-specific overrides (system prompt addendum, scoped history)
- Constrains resources (model allowlist, token budget)

The child runs with its own kernel. Its context evolves independently.
Results flow back to the parent via a handle — the parent wrote a fork
request, got a handle, reads the handle for the child's output. Same
handle pattern as everything else.

## Context as Portable Bundle

The context bundle is the agent's complete identity and state. Because
context is a system of providers (not a bag of data), serialization
captures the provider state, not the synthesized prompt.

To move an agent between hosts:

1. **Serialize:** Read each mount's `snapshot/state`. Providers serialize
   their own state — history serializes messages, file-span providers
   serialize their span references, lazy providers serialize their
   configuration (not their computed values).
2. **Ship:** Transfer the bundle to the target host.
3. **Restore:** Write each mount's `snapshot/state` into a fresh namespace.
   Providers reconstruct themselves from serialized state. Lazy providers
   re-attach to local resources on the target host.
4. **Execute:** Start a kernel with that namespace.

The kernel binary is the same everywhere. The context is what varies.
A context serialized from a local CLI session can be resumed on a remote
server, and vice versa. The Wasm module is deterministic — same context
in, same behavior out.

### What's in the bundle

| Mount | Contents | Portable? |
|-------|----------|-----------|
| `system/` | System prompt | Yes |
| `history/` | Conversation messages (provider state, not the view) | Yes |
| `context/*` | Provider configurations, span refs, pinned entries | Yes (refs may need re-resolution on target) |
| `gate/` | Model config, account names | Yes (keys excluded) |
| `tools/schemas` | Available tool definitions | Yes |
| `tools/exec/*` | Completed handle results | Yes |
| `tools/exec/*` (pending) | In-flight handles | No — re-issued on restore |

API keys are never serialized. The target host provides its own keys via
its gate configuration. Account names are portable (the agent refers to
"anthropic" or "openai"); key resolution happens at the host level.

Span references (file paths, line ranges) are portable as data but may
need re-resolution on the target host if the filesystem differs. The
provider handles this — if a referenced file doesn't exist at the target,
the span resolves to an empty value or an error, not a crash.

## Policy Enforcement

The host enforces policy at two levels, both transparent to the kernel:

**PolicyStore** wraps the ToolStore. Every tool write passes through
`PolicyCheck` before reaching the tool module. The kernel doesn't know
policy exists — a denied tool write returns a `StoreError`, which the
kernel treats as a tool execution failure.

**ClashSandboxPolicy** wraps subprocess execution. Each tool invocation
gets an ephemeral OS-level sandbox (sandbox-exec on macOS, Landlock on
Linux). The kernel doesn't know sandboxing exists — it writes a tool
request and reads the result.

Policy is a host concern, not a kernel concern. Different hosts can enforce
different policies on the same context bundle.
