# TODO

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

## Async tool execution via StructFS handles

Tool execution should follow the native StructFS async pattern: writes return
handles, reads resolve them, no hidden state accumulation.

**Design:**

```
h1 = write("tools/read_file", input)   // returns handle "exec/001" immediately
h2 = write("tools/shell", input)       // returns handle "exec/002" immediately
batch = write("tools/await", [h1, h2]) // returns batch handle "await/003"
results = read(batch)                  // blocks until all resolve
```

The agent holds handles as values. When it wants results, it writes the set of
handles it cares about to `tools/await`, gets a batch handle back, reads that.

**What needs to change:**

1. `sandboxed_exec` splits into `sandboxed_spawn` (returns JoinHandle, non-blocking)
   and `sandboxed_await` (blocks on JoinHandle). ToolStore needs a runtime handle
   to spawn blocking tasks.
2. ToolStore write returns an execution handle path instead of blocking during
   execution. The handle is a future/JoinHandle under the hood.
3. `tools/await` path accepts a set of handles, returns a composite handle.
   Reading the composite blocks until all constituent handles resolve.
4. TurnStore as a stateful queue/registry goes away. The handles ARE the state.
5. Wasm agent loop simplifies: fire all tool writes, collect handles, write
   handles to await, read batch result, write to history.

**Current state:** TurnStore exists as a queue but adds no value — the Wasm
agent manages execution directly and TurnStore is write-only bookkeeping.
Remove current TurnStore usage from the agent loop as a first step, then
implement the handle-based async model.

## Dead code in GateStore

`GateStore::completion_tool_schemas()` and the `gate/tools/schemas` read path
are dead code — nothing reads that path anymore. Tool schemas are served by
`ToolStore` at `tools/schemas`. Remove `completion_tool_schemas()` and the
`"tools"` arm in `GateStore::read()`.

## Kernel step() method

A `Kernel::step(context, outcomes) -> Vec<ToolEffect>` that makes the kernel a
pure state machine. With handle-based async execution, step() returns tool call
intents, the host fires writes to get handles, awaits them, and feeds outcomes
back to the next step(). Evaluate after the async handle model lands.
