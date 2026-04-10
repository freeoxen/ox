# TODO

## Agent as pico-process, namespace as context

The agent is a long-running process (pico-process in Wasm). The host provides
the namespace — a structured filesystem. All effects are syscalls: the agent
writes a request, gets a handle back, reads the handle for the result. The host
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

## Kernel speaks handles natively

The Kernel IS the agent loop — it runs inside the Wasm pico-process. But its
current three-phase API (`initiate_completion`, `consume_events`,
`complete_turn`) assumes one completion per turn. Multi-completion turns
(N completions to different models/accounts in parallel) need a handle-native
API where completions and tools are both handles.

The Kernel stays. The three-phase ceremony gets replaced with handle-aware
methods that can fire multiple completions and tool calls per turn.

## Uniform handle model for all effects

Everything is a handle. Completions, tools, native tools — all the same
pattern. The agent writes a request, gets a handle, reads the handle for
the result. The host provides handle resolution.

**Design:**
```
// Multi-completion turn:
c1 = write("tools/completions/complete/anthropic", prompt_1)
c2 = write("tools/completions/complete/openai", prompt_2)
batch = write("tools/await", [c1, c2])
responses = read(batch)

// Process responses, extract tool calls from each
t1 = write("tools/shell", ...)
t2 = write("tools/read_file", ...)
tool_batch = write("tools/await", [t1, t2])
results = read(tool_batch)

write("history/append", all_results)
```

**What needs to change:**
1. CompletionModule write returns a handle immediately. Completion executes
   on a background thread (or tokio task). Reading the handle blocks until
   the streaming response completes.
2. `sandboxed_exec` splits into spawn + await. Tool writes return handles.
3. Native tools execute in-process but still return handles (instant resolve).
4. `tools/await` accepts a set of handles, returns a composite handle.
5. TurnStore goes away. Handles ARE the state.
6. Completed handles persist to namespace immediately (crash safety).

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
