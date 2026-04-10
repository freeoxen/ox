# TODO

## Agent as pico-process, namespace as context

The agent is a long-running process (pico-process in Wasm). The host provides
the namespace — a structured filesystem. Tool calls are syscalls. The host
never drives the agent; the agent drives itself.

The namespace IS the agent's context — not just history, but the full structured
state: system prompt, conversation history, tool configuration, model config,
in-flight execution handles. Context is what gets serialized, shipped, restored.
History is one store in the context, not the context itself.

**Observability:** Read the namespace. The agent's writes ARE the observable
state. No polling, no status queries — read the filesystem.

**Resumability:** Serialize the context (full namespace). Spawn a new agent
instance with that context. The agent reads its context, sees where things
stand, picks up. Mid-turn state (pending tool calls, partial results) must
be in the namespace, not in local variables or the Wasm call stack.

**Portability:** The context is the bundle. Serialize, ship to remote host,
deserialize, spawn agent. Pull back local, same thing. The Wasm module is
the same everywhere. The context is the only variable.

**What's blocking:**
1. Kernel state is in Rust fields, not the namespace. Should be derivable
   from or stored in the context.
2. Tool results aren't persisted until the full batch completes — partial
   progress lost on crash. Each completed handle should write its result
   to the namespace immediately.
3. The snapshot system covers system/history/gate but not tool execution
   state. Extend to cover the full namespace.

## Async tool execution via StructFS handles

Tool execution should follow the native StructFS async pattern: writes return
handles, reads resolve them. No hidden state accumulation. Tool calls are
syscalls — the agent writes a request, the host handles it, the agent reads
the result.

**Design:**

```
h1 = write("tools/read_file", input)   // syscall: returns handle immediately
h2 = write("tools/shell", input)       // syscall: returns handle immediately
batch = write("tools/await", [h1, h2]) // syscall: returns batch handle
results = read(batch)                  // blocks until all resolve
```

The agent holds handles as values. When it wants results, it writes the set of
handles it cares about to `tools/await`, gets a batch handle back, reads that.

**What needs to change:**

1. `sandboxed_exec` splits into `sandboxed_spawn` (returns JoinHandle) and
   `sandboxed_await` (blocks on JoinHandle). ToolStore needs a runtime handle.
2. ToolStore write returns an execution handle path instead of blocking.
3. `tools/await` accepts a set of handles, returns a composite handle. Reading
   the composite blocks until all constituent handles resolve.
4. TurnStore as a stateful queue goes away. The handles ARE the state.
5. Completed handles write results to the namespace immediately (crash safety).

**Current state:** TurnStore exists as a queue but adds no value — the Wasm
agent manages execution directly and TurnStore is write-only bookkeeping.
Remove current TurnStore usage from the agent loop as a first step, then
implement the handle-based async model.

## Per-thread model allowlists

Threads should be constrainable to a subset of models/accounts. The Cascade
(thread overrides → global config) already exists, but there's no enforcement —
a thread can write any model to its local overrides. Need an allowlist mechanism
on CompletionModule or PolicyCheck that rejects completion requests for
unauthorized models.

## Completion path policy enforcement

LLM API calls (`completions/*`) are skipped in CliPolicyCheck. The agent makes
API calls freely with no approval gate, no spend limit, no rate control.
CliCompletionTransport uses reqwest directly (not a subprocess), so
ClashSandboxPolicy never sees it either. Consider whether budget/rate-limit
checks belong here.

## Dead code in GateStore

`GateStore::completion_tool_schemas()` and the `gate/tools/schemas` read path
are dead code — nothing reads that path anymore. Tool schemas are served by
`ToolStore` at `tools/schemas`. Remove `completion_tool_schemas()` and the
`"tools"` arm in `GateStore::read()`.
