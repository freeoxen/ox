# Life of an Agent

This document traces an agent from birth to quiescence, covering startup,
the main loop, streaming, multi-completion turns, tool execution, context
management, fork, crash recovery, and resumption.

Prerequisite reading: [Agent Execution Model](agent-execution-model.md)
for the core abstractions (agent = context bundle, kernel = executor,
host = namespace provider, all effects are handles).

---

## 1. Birth: Context Creation

An agent begins as an empty context bundle. The host creates a namespace
and mounts the initial providers:

```
namespace = Namespace::new()
namespace.mount("system",  SystemProvider("You are a coding assistant."))
namespace.mount("history", HistoryProvider::new())
namespace.mount("tools",   ToolStore::new(fs, os, completions))
namespace.mount("gate",    GateStore::new().with_config(config))
namespace.mount("context", ContextManager::new())
```

At this point the agent exists but has never run. Its context contains a
system prompt, empty history, tool schemas, and model configuration. No
messages, no state, no handles.

## 2. Bootstrap: First Input

The host writes the user's first message to the context:

```
write("history/append", {role: "user", content: "Fix the bug in auth.rs"})
```

Then the host starts the kernel. On the CLI, this means loading the Wasm
module and calling its `run()` export. The kernel begins executing inside
the Wasm pico-process, with the namespace as its filesystem.

## 3. Startup: Reading Context

The kernel's first act is reading its context to determine where it is.
This is the same code path for fresh starts and crash recovery — there is
no separate "recovery mode."

```
let messages = read("history/messages")
let last = messages.last()

match last.role {
    "user" => {
        // Normal: user message waiting for response. Proceed to completion.
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
        // Proceed to completion (tool results are already in history).
    }
}
```

The kernel derives its position entirely from context. There is no saved
instruction pointer, no phase enum, no "where was I" state. The context
IS the state. Startup and recovery are the same code.

## 4. The Loop

```
loop {
    // Read context: what accounts are available, what's the current state
    let accounts = read("gate/accounts")
    let prompt = read("prompt")

    // The kernel decides what completions to fire.
    // One model? Three for consensus? A cheap model for planning
    // and an expensive one for synthesis? That's the agent's intelligence.
    let handles = decide_and_fire_completions(accounts, prompt)

    // Await all completions
    let batch = write("tools/await", handles)
    let responses = read(batch)

    // Process responses, write assistant message(s) to history
    let tool_calls = process_and_record(responses)

    if tool_calls.is_empty() {
        return  // No tools requested. Turn is done.
    }

    // Fire tool calls, await results, record to history
    execute_and_record(tool_calls)

    // Loop: tool results are now in history, so the next read("prompt")
    // includes them, and the models see the results.
}
```

The kernel reads its context and decides what to do. It might fire one
completion or ten. It might use one model or five. That decision is the
kernel's reasoning about the task — not a framework choice. The loop
is the same regardless: read context, fire completions, process
responses, maybe fire tools, record everything, loop.

## 5. Streaming: Real-Time Text Delivery

A completion handle doesn't just block-then-return. Streaming completions
produce incremental text deltas that must reach the TUI in real time.

The host handles this transparently. When the kernel writes to a
completion path, the host's CompletionTransport fires the HTTP request
with `stream: true`. As SSE events arrive, the host:

1. Writes each text delta to `events/emit` (TUI sees it immediately)
2. Accumulates events into the handle's result buffer
3. When the stream ends, marks the handle as resolved

From the kernel's perspective, `read(handle)` blocks until all events are
accumulated and then returns the complete response. But from the user's
perspective, text streams to the screen as it arrives. The kernel doesn't
manage streaming — the host does, as a side effect of handle resolution.

For multi-completion turns where N completions stream simultaneously, the
host interleaves events from all active streams. Each completion's events
are tagged with their handle ID so the TUI can attribute text to the
right source.

```
// Kernel perspective: fire N completions and wait
let handles = [h1, h2, h3]  // however many the kernel decided to fire
let batch = write("tools/await", handles)
let responses = read(batch)  // blocks until all complete

// Host perspective: while the kernel blocks, the host is:
// - streaming each handle's text deltas to events/emit
// - accumulating events into each handle's result buffer
// - when all finish, unblocking the kernel's read
```

The kernel never sees individual stream events. It gets the complete
response. The host delivers the real-time experience.

## 6. Tool Execution: Handles In Practice

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

    // Emit result events and record to history
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

### Await Variants

`tools/await` blocks until ALL handles resolve. This is the right default
for tool calls — the model needs all results before it can continue.

For cases where the kernel wants to process results as they arrive:

- **`tools/await_any`** — returns the first handle that resolves, plus
  its result. The kernel can process it and re-issue `await_any` for the
  remaining handles. Useful for mixed-latency workloads where one fast
  result enables further work while slow results are still pending.

- **`tools/await_each`** — returns a handle that resolves repeatedly,
  once per completed constituent. The kernel reads it N times for N
  handles, getting results in completion order. Useful for progress
  reporting and streaming-style processing.

```
// Process results as they complete, not all-at-once
let pending = [h1, h2, h3]
let each = write("tools/await_each", pending)

for _ in 0..pending.len() {
    let result = read(each)  // returns next completed result
    process_incrementally(result)
}
```

## 7. Multi-Completion Turn: The General Case

The single-completion turn is the degenerate case. The general case: the
kernel reads its context, decides it needs input from multiple models,
constructs multiple prompt Values, and fires them all.

### Example: Consensus

The kernel reads available accounts and decides to fan out for review:

```
let accounts = read("gate/accounts")
let system = read("system")
let history = read("history/messages")

let review_prompt = {
    system: system + "\nReview this code for correctness.",
    messages: history,
    tools: [],
}

// The kernel picks which accounts to use based on what's available,
// what the task needs, what the budget allows
let handles = accounts.iter()
    .filter(|a| useful_for_review(a))
    .map(|a| write(&format!("tools/completions/complete/{}", a.name), review_prompt))
    .collect()

let batch = write("tools/await", handles)
let responses = read(batch)
let consensus = analyze_responses(responses)
```

### Example: Decomposition

The kernel splits a task across multiple completions:

```
let accounts = read("gate/accounts")
let fast = pick_account(accounts, "fast")

let c1 = write(&format!("tools/completions/complete/{fast}"), {
    system: "Write unit tests for auth.rs",
    messages: [file_span("src/auth.rs")],
})
let c2 = write(&format!("tools/completions/complete/{fast}"), {
    system: "Write integration tests for auth.rs",
    messages: [file_span("src/auth.rs"), file_span("tests/helpers.rs")],
})

let batch = write("tools/await", [c1, c2])
let [unit_tests, integration_tests] = read(batch)
```

### History for Multi-Completion Turns

When the kernel fires N completions, the responses don't all become
separate assistant messages in history. The kernel decides what to
record. Options:

**Synthesize:** The kernel reads all N responses, synthesizes a single
coherent response (possibly using another completion to merge), and
writes one assistant message to history. The N individual responses are
intermediate computation — they don't appear in history. The model sees
a clean conversation on the next turn.

**Attribute:** The kernel writes a structured assistant message that
attributes content to sources:

```
write("history/append", {
    role: "assistant",
    content: [
        {type: "text", text: "Based on review from three models:\n..."},
        {type: "metadata", sources: ["anthropic/sonnet", "openai/gpt-4o", "anthropic/haiku"]},
    ]
})
```

**Branch and merge:** The kernel forks sub-agents for each completion
(see Fork below), and their results merge back as tool results rather
than assistant messages. History shows one assistant message that
requested the fork, plus the merged results.

The right choice depends on the kernel's reasoning strategy. The
framework doesn't prescribe it — history is what the kernel writes to it.

## 8. Context Management Within the Loop

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

## 9. Fork: Spawning Sub-Agents

When the kernel needs to delegate work, it forks. Fork is a
context-to-context transform — the child inherits pieces of the
parent's context cheaply without re-synthesis.

```
let child_handle = write("tools/fork", {
    system: "You are a test writer. Write tests for the given code.",
    inherit: {
        context: ["auth-source"],  // inherit pinned spans by ID
        tools: ["read_file", "write_file", "shell"],
        gate: ["anthropic"],
    },
    task: "Write comprehensive tests for src/auth.rs",
})

// The child runs with its own kernel, its own context, its own loop.
// The parent continues — child execution is a handle like anything else.
let child_result = read(child_handle)
```

The child's context is derived from the parent's context by the
transform. File spans are shared references, not copies — the child
sees the same data without re-reading files or re-prompting an LLM.
Tool schemas are filtered, not regenerated. The system prompt is
composed, not re-synthesized.

The child is a full agent with its own kernel. It can fire its own
completions, use its own tools, manage its own context. Its results
flow back to the parent through the handle. The host decides where the
child runs — same thread, different thread, different machine. The
parent doesn't know or care.

## 10. Quiescence: Turn Complete

The kernel exits when it has nothing left to do — the model responded
with text only (no tool calls) and the kernel has no further work.

```
let tool_calls = extract_tool_calls(content)
if tool_calls.is_empty() {
    write("events/emit", {type: "turn_end"})
    return
}
```

The kernel's `run()` function returns. The Wasm pico-process exits. The
host reads the context — history now contains the full conversation
including the model's final response. The context is the result.

The host persists the context to disk (snapshot). The agent is now
suspended data. It can be:

- **Resumed** — user sends another message, host appends to history,
  starts a new kernel with the same context.
- **Shipped** — context serialized and sent to a remote host.
- **Forked** — another agent inherits this context as a starting point.
- **Inspected** — read the namespace to see everything that happened.

## 11. Resumption: Next User Message

When the user sends another message, the host appends it to history and
starts a new kernel with the existing context:

```
write("history/append", {role: "user", content: "Now optimize it"})
kernel.run(context)
```

The kernel reads history, sees the new user message at the end, and
proceeds to completion. The full conversation history is available to
the model. The kernel doesn't know this is the second turn or the
hundredth — it reads context and acts.

## 12. Crash Recovery in Detail

If the process dies mid-execution, the context on disk reflects the last
successful persistence point. On restart the host restores context from
disk and starts a new kernel. The kernel reads history and detects where
things stand. There is no "recovery mode" — startup IS recovery.

**Case A: Crash during completion.** Last message is `user`. The
completion never started or never completed. The kernel proceeds
normally (fires a new completion). No data loss — the completion
response wasn't recorded yet. The user may see duplicate streaming
text if the TUI showed partial streaming before the crash; this is
a cosmetic issue, not a data integrity issue.

**Case B: Crash after completion, before tools.** Last message is
`assistant` with `tool_use` blocks, no `tool_result` follows. The
kernel re-issues all tool calls. Idempotency is the tool's
responsibility — a `write_file` tool should produce the same result
if called twice with the same input; a `shell` tool running `rm -rf`
cannot be safely re-issued. The kernel's contract: if context says a
tool call was requested but no result was recorded, the call gets
re-issued.

**Case C: Crash during tool execution.** If individual handle results
persist to context as they complete (not batched at end-of-turn), the
kernel detects which tools finished and which didn't. It re-issues only
the incomplete ones. If results are not persisted mid-batch, this
degrades to Case B.

**Case D: Crash after tools, before next completion.** Last message is
`tool_result`. The kernel reads the prompt (which includes the tool
results) and fires the next completion. No re-execution needed.

The kernel never needs to know it crashed. It reads context, acts on
what it finds.

---

## Summary: The Agent Lifecycle

```
  Birth          Bootstrap        Startup          The Loop
  (empty ctx) -> (user msg) -> (read ctx) -> (completion -> tools -> record) -+
                                    ^                                         |
                                    |    Quiescence    Resumption             |
                                    +-- (suspend) <-- (new user msg)          |
                                    |                                         |
                                    +-- (crash) -> (restore ctx) -> (read) ---+
                                    |
                                    +-- (ship) -> (remote host) -> (read ctx) -> ...
```

At every point, the context is the truth. The kernel is stateless.
The host provides the filesystem. Handles are the universal effect
mechanism. Values are the universal data type.
