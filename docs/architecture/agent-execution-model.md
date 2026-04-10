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
c1 = write("tools/completions/complete/anthropic", prompt_value_1)
c2 = write("tools/completions/complete/openai", prompt_value_2)
t1 = write("tools/read_file", {path: "src/lib.rs"})
t2 = write("tools/shell", {command: "cargo test"})

batch = write("tools/await", [c1, c2, t1, t2])
results = read(batch)
```

Writes return handles immediately. The host executes effects asynchronously.
Reads block until the handle resolves. `tools/await` accepts any set of
handles and returns a composite handle. `tools/await_any` returns the first
to complete. `tools/await_each` yields results in completion order.

### The Prompt Is a Value

There is no `CompletionRequest` type at the kernel boundary. The kernel
deals in `Value` — StructFS's native data type. `read("prompt")` is a
convenience provider that assembles a completion-ready Value. The kernel
can also read context components directly and construct its own Values
for multi-completion turns.

The `prompt` path is sugar, not architecture. The architecture is: read
Values from context, write Values to effect paths, read handles for
results.

## Life of an Agent

See [Life of an Agent](life-of-an-agent.md) for the detailed walkthrough:
birth, bootstrap, startup/recovery, the main loop, streaming, tool
execution, multi-completion turns, context management, fork, quiescence,
resumption, and crash recovery.

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

- **Pin a file span** — `write("context/pin", {path, start, end})`
- **Drop a reference** — `write("context/drop", {ref_id})`
- **Summarize** — `write("context/summarize", {ref_id})`
- **Window history** — `write("context/history/window", {last_n})`

The kernel is an active participant in context management, not a passive
consumer. This is loop resolution — the kernel tunes its own prompt as
part of its reasoning process.

### Fork: Context-to-Context Transform

When the kernel forks a sub-agent, it produces a transform from parent
context to child context. The child inherits pieces cheaply — file spans,
tool schemas, system prompt fragments — without re-synthesizing through
an LLM.

```
parent_context ──transform──> child_context
     │                            │
     │ file spans: inherited      │ file spans: same refs
     │ history: filtered/none     │ history: empty or subset
     │ tools: subset              │ tools: scoped subset
     │ system: parent + task      │ system: derived prompt
     │ gate: parent accounts      │ gate: subset of accounts
```

The child runs with its own kernel. Results flow back via a handle.

## Context as Portable Bundle

Because context is a system of providers (not a bag of data),
serialization captures provider state, not the synthesized prompt.

1. **Serialize:** Read each mount's `snapshot/state`.
2. **Ship:** Transfer the bundle.
3. **Restore:** Write each mount's `snapshot/state` into a fresh namespace.
4. **Execute:** Start a kernel. It reads context, picks up.

| Mount | Contents | Portable? |
|-------|----------|-----------|
| `system/` | System prompt | Yes |
| `history/` | Messages (provider state, not the view) | Yes |
| `context/*` | Span refs, pins, provider configs | Yes (refs re-resolve on target) |
| `gate/` | Model config, account names | Yes (keys excluded) |
| `tools/schemas` | Tool definitions | Yes |
| `tools/exec/*` | Completed handle results | Yes |
| `tools/exec/*` (pending) | In-flight handles | No — re-issued on restore |

## Policy Enforcement

The host enforces policy at two levels, both transparent to the kernel:

**PolicyStore** wraps the ToolStore. Every tool write passes through
`PolicyCheck` before reaching the tool module. A denied write returns a
`StoreError` — the kernel treats it as a tool failure.

**ClashSandboxPolicy** wraps subprocess execution. Each invocation gets
an ephemeral OS-level sandbox. The kernel doesn't know sandboxing exists.

Policy is a host concern, not a kernel concern. Different hosts can enforce
different policies on the same context bundle.
