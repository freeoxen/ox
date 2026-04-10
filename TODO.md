# TODO

## Namespace-as-complete-state (observability, resumability, portability)

The namespace should be the agent's complete state. The agent's execution
position should be derivable from reading the namespace, not from the Wasm
call stack or Rust struct fields.

**Observability:** Read the namespace to see what the agent is doing. History
shows what happened. Pending handles show what's in flight. The kernel's phase
is derivable from the last message (assistant with tool_use blocks but no tool
results = tools pending).

**Resumability:** Restore namespace from disk, call `step()`. The kernel reads
history, sees where things stand, picks up. No Wasm stack to reconstruct.
Currently, mid-turn crashes lose partial results — each completed handle should
persist its result immediately.

**Portability:** Serialize the namespace (all stores are Readers). That's the
bundle. Ship to remote, deserialize, `step()`. Pull back, same. The Wasm module
is the same everywhere.

**What's blocking:**
1. Kernel state is in Rust fields, not StructFS. Should be derivable from or
   stored in the namespace.
2. Execution position is in the Wasm call stack. The three-phase loop should
   become a pure `step()` function.
3. Tool results aren't persisted until the full batch completes — partial
   progress lost on crash.
4. The Wasm agent IS the loop. The host should drive the loop; the Wasm module
   should be a pure step function.

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

1. `sandboxed_exec` splits into `sandboxed_spawn` (returns JoinHandle) and
   `sandboxed_await` (blocks on JoinHandle). ToolStore needs a runtime handle.
2. ToolStore write returns an execution handle path instead of blocking.
3. `tools/await` accepts a set of handles, returns a composite handle. Reading
   the composite blocks until all constituent handles resolve.
4. TurnStore as a stateful queue goes away. The handles ARE the state.
5. Wasm agent loop simplifies: fire writes, collect handles, write to await,
   read batch result, write to history.

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

## Kernel step() method

A pure `Kernel::step(namespace) -> Vec<Effect>` that derives the next action
from the namespace state. With handle-based async execution, step() returns
intents, the host fires writes to get handles, awaits them, and persists
results. The host drives the loop; the kernel is a pure function. This is the
convergence point of observability, resumability, and portability — the loop
becomes:

```
loop {
    effects = step(namespace)
    if effects.is_empty() { break }
    handles = fire(effects)
    batch = await(handles)
    namespace.write(batch.results)
    // crash-safe: namespace has results, step() picks up
    // portable: serialize namespace, ship, deserialize, step()
    // observable: read namespace, everything is there
}
```
