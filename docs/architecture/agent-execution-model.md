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
│  system/     history/     tools/     gate/               │
│  ┌───────┐   ┌───────┐   ┌──────┐   ┌──────┐           │
│  │prompt │   │messages│   │schemas│  │config│            │
│  │       │   │       │   │exec/* │   │accts │           │
│  └───────┘   └───────┘   └──────┘   └──────┘           │
│                                                         │
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
c1 = write("tools/completions/complete/anthropic", prompt_1)
c2 = write("tools/completions/complete/openai", prompt_2)

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

### Multiple Completions Per Turn

The handle model makes multi-completion turns natural. The kernel can fire N
completions to different models or accounts in a single turn, await all of
them, and synthesize the results. This is the expected default, not an edge
case.

```
// Fan out to three models for consensus
c1 = write("tools/completions/complete/anthropic", prompt)
c2 = write("tools/completions/complete/openai", prompt)
c3 = write("tools/completions/complete/local", prompt)
batch = write("tools/await", [c1, c2, c3])
responses = read(batch)

// Synthesize, extract tool calls from all responses, fire tools
```

The old three-phase Kernel API (`initiate_completion`, `consume_events`,
`complete_turn`) assumed one completion per turn. The handle model has no
such constraint — the kernel fires as many writes as it wants and awaits
them in any combination.

## The Kernel Loop

The kernel runs as a pico-process inside the Wasm sandbox. It drives its own
loop. The host never calls into the kernel to advance it — the kernel calls
out to the host via StructFS reads and writes.

```
fn kernel_main(context: &mut dyn Store) {
    // On startup, read context to determine position.
    // If last assistant message has tool_use blocks with no
    // tool results following, we crashed mid-turn — recover.
    let pending = detect_pending_from_context(context);
    if !pending.is_empty() {
        execute_and_record(context, &pending);
    }

    loop {
        // Read prompt from context (synthesized from all stores)
        let prompt = context.read(&path!("prompt"));

        // Fire completions — could be one or many
        let handles = fire_completions(context, &prompt);
        let batch = context.write(&path!("tools/await"), handles);
        let responses = context.read(&batch);

        // Process responses, write assistant messages
        let tool_calls = process_and_record(context, responses);

        if tool_calls.is_empty() {
            return; // Turn complete, no tools requested
        }

        // Fire tool calls
        execute_and_record(context, &tool_calls);
    }
}
```

### Crash Recovery

The kernel reads context on startup to detect incomplete turns. The context
is the truth — if the last message in history is an assistant message with
`tool_use` blocks and no corresponding tool result message follows, the
kernel knows it crashed mid-execution.

Recovery is handle-based. Completed handles persist their results to the
context immediately upon completion (not batched at end-of-turn). On
restart, the kernel checks which handles completed and which didn't,
re-issues only the incomplete ones.

For tools with side effects (file writes, shell commands), idempotency is
the tool's responsibility, not the kernel's. The kernel's contract is: if
the context says a tool call happened but no result was recorded, the tool
call gets re-issued. Tools that can't be safely re-executed should be
designed to detect and skip duplicate invocations.

### Observability

The host reads the context to observe the kernel. The kernel's writes to
the context ARE the observable state:

- `history/messages` — what the agent said and what tools returned
- `tools/exec/*` — in-flight execution handles (what's running right now)
- `events/emit` — real-time TUI events (streaming text, tool progress)

No polling. No status queries. Read the namespace.

## Context as Portable Bundle

The context bundle is the agent's complete identity and state. To move an
agent between hosts:

1. **Serialize:** Read the full namespace (every mount's `snapshot/state`).
   This produces a self-contained bundle — JSON, CBOR, or any format.
2. **Ship:** Transfer the bundle to the target host (file, network, etc.).
3. **Restore:** Write each mount's `snapshot/state` into a fresh namespace.
4. **Execute:** Start a kernel with that namespace. It reads context,
   determines position, picks up.

The kernel binary is the same everywhere. The context is what varies.
A context serialized from a local CLI session can be resumed on a remote
server, and vice versa. The Wasm module is deterministic — same context
in, same behavior out.

### What's in the bundle

| Mount | Contents | Portable? |
|-------|----------|-----------|
| `system/` | System prompt | Yes |
| `history/` | Conversation messages | Yes |
| `gate/` | Model config, account names | Yes (keys excluded) |
| `tools/schemas` | Available tool definitions | Yes |
| `tools/exec/*` | Completed handle results | Yes |
| `tools/exec/*` (pending) | In-flight handles | No — re-issued on restore |

API keys are never serialized. The target host provides its own keys via
its gate configuration. Account names are portable (the agent refers to
"anthropic" or "openai"); key resolution happens at the host level.

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
