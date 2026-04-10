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

## TurnStore as a first-class state machine

TurnStore currently tracks enqueue/results lifecycle but the Wasm agent still
manages the per-tool execution loop itself. The end state is the agent loop
collapsing to `read turn/pending → write turn/execute → read turn/results`,
with TurnStore dispatching through the namespace. Blocked on a clean way for
TurnStore to write through the namespace without store-calling-store gymnastics
(needs a broker handle or similar indirection).

## Dead code in GateStore

`GateStore::completion_tool_schemas()` and the `gate/tools/schemas` read path
are dead code — nothing reads that path anymore. Tool schemas are served by
`ToolStore` at `tools/schemas`. Remove `completion_tool_schemas()` and the
`"tools"` arm in `GateStore::read()`.

## Kernel step() method

A `Kernel::step(context, outcomes) -> Vec<ToolEffect>` that makes the kernel a
pure state machine. May be redundant if TurnStore drives everything. Evaluate
after TurnStore reaches its end state.
