# ox-kernel: Agent State Machine

## Overview

ox-kernel is the event-driven state machine at the center of Ox. It thinks
purely in terms of **events** — structured read and write operations following
[StructFS](https://github.com/StructFS/structfs) semantics. The kernel does not
think in terms of "messages" or "prompts." It receives events from the outside
world, processes them through its state machine, emits events back out, and
manipulates ox-context to construct prompts that drive the LLM.

ox-kernel has **zero** platform-specific dependencies — no networking, no
filesystem, no allocator requirement beyond what tools themselves need. All I/O
is expressed as StructFS read/write events that cross the kernel boundary.
ox-core is the composition layer that wires these events to concrete systems.

## Events as the Fundamental Unit

Everything the kernel sees and produces is a StructFS-style read or write on a
path. Inbound events arrive from the host or the LLM. Outbound events are
directed at ox-context, ox-shell, tools, or the host.

Examples:

| Direction | Path | Operation | Meaning |
|-----------|------|-----------|---------|
| Inbound | `/user/input` | write | User submits a prompt |
| Inbound | `/llm/stream` | write | Token arrives from transport |
| Inbound | `/tool/result/{id}` | write | Tool execution completes |
| Outbound | `/context/prompt` | read | Kernel requests assembled prompt |
| Outbound | `/context/history` | write | Kernel appends an event to the log |
| Outbound | `/transport/complete` | write | Kernel sends completion request |
| Outbound | `/tool/exec/{name}` | write | Kernel invokes a tool |

The kernel does not interpret the content of these events beyond what is needed
to drive state transitions. The semantics of any given path are defined by the
provider mounted at that path in ox-context.

## Agent State Machine

### States

```
              event in                  tool calls              tools complete
  [Idle] ──────────────> [Streaming] ──────────────> [Executing] ──────────────┐
    ^                        │                           │                     │
    │         abort          │             abort          │                     │
    │◄───────────────────────┘◄──────────────────────────┘                     │
    │                                                                          │
    └──────────────────────────────────────────────────────────────────────────┘
```

| State | Description | Allowed Operations |
|-------|-------------|--------------------|
| **Idle** | Not running. Ready for input. | Accept events, resume, mutate state |
| **Streaming** | Awaiting LLM response via transport. | Abort, steer |
| **Executing** | Running tool call(s). | Abort, steer |

Transitions are enforced statically where possible, and at runtime otherwise.
Submitting an event while not Idle is an error, not a queue (except for steers
and follow-ups, which are explicitly queued).

## The Agentic Loop

The loop is the heart of the kernel. It is **not recursive** — it is an
explicit loop with well-defined exit conditions.

```
inbound event (e.g. write to /user/input)
  │
  ▼
┌──────────────────────────────────────────────────┐
│ 1. Write event to ox-context (history provider)  │
│ 2. Read /context/prompt from ox-context          │
│    (ox-context synthesizes the prompt from all   │
│     its providers: history, documents, tools,    │
│     system config, etc.)                         │
│ 3. Write completion request to transport         │
│ 4. Receive stream events from transport          │◄─────────┐
│ 5. Write assistant response to ox-context        │          │
│ 6. Check stop reason:                            │          │
│    ├─ ToolUse → write to /tool/exec/* ───────────┼──────────┘
│    ├─ EndTurn → return to Idle                   │
│    ├─ MaxTokens → return to Idle                 │
│    └─ Abort → return to Idle                     │
└──────────────────────────────────────────────────┘
```

Step 2 is the critical interaction between the kernel and ox-context. The
kernel reads the assembled prompt — ox-context synthesizes it from all mounted
providers (history, reference documents, tool descriptions, session metadata,
etc.). The kernel neither knows nor cares how the prompt was assembled; it just
reads the result.

### Tool Execution

When the LLM response contains tool calls:

1. Validate each tool call against the registry (unknown tool → error result)
2. Validate parameters against the tool's schema (invalid params → error result)
3. Execute all valid tool calls **concurrently** (the executor is injected —
   could be cooperative, threaded, or sequential)
4. Write tool results back to ox-context (history provider)
5. Loop back to step 2 (read updated prompt from ox-context)

Tools that panic or exceed a timeout produce error results rather than crashing
the agent.

### Steering and Follow-ups

Two mechanisms for injecting events during execution:

- **Steer** — Interrupts after the current tool batch completes. The injected
  event is written to ox-context before the next prompt read. Use case: user
  correction mid-run.
- **Follow-up** — Queued until the agent returns to Idle, then automatically
  triggers a new loop iteration. Use case: chaining requests.

Both are ordered FIFO within their category. Steers take priority over
follow-ups.

## Observability Events

Separately from the StructFS events that drive the state machine, the kernel
emits observability events for subscribers. These are notifications, not
read/write operations:

- **Agent lifecycle** — start, end (with optional error)
- **Turn lifecycle** — turn start, turn end (with stop reason)
- **Stream lifecycle** — stream start, stream delta, stream end
- **Tool execution** — tool start, tool progress update, tool end (with result)

Subscribers are synchronous callbacks — the kernel does not spawn tasks for
event delivery.

## Tools

Tools are the kernel's mechanism for acting on the world. A tool exposes:

- **Name** — Unique identifier the LLM uses to invoke it.
- **Label** — Human-readable display name.
- **Description** — Guidance for the LLM on when and how to use the tool.
- **Parameter schema** — JSON Schema describing expected inputs.
- **Execute** — The implementation. Receives validated parameters, a
  cancellation signal, and an update callback for streaming partial results.
  Returns content for the LLM and optional details for the host/UI (not sent
  to the LLM).

The **tool output** separates what the LLM sees (content) from what the host
sees (details). This allows tools to return rich UI data without bloating the
LLM context.

Tools are registered at construction or swapped at runtime while Idle. The
registry produces the set of tool schemas that ox-context uses when assembling
prompts.

## Interfaces

The kernel has two injected dependencies:

| Interface | Provided by | Purpose |
|-----------|-------------|---------|
| Transport | ox-shell | LLM communication |
| Context | ox-context | Structured data namespace and prompt synthesis |

### Transport

Accepts a completion request and returns a stream of events. The kernel
consumes stream events and never constructs HTTP requests, parses SSE, or
handles provider-specific serialization. That is entirely ox-shell's
responsibility.

### Context

The kernel's interface to all structured data. The kernel writes events into
ox-context and reads synthesized prompts out of it. ox-context is a StructFS-
style namespace of providers — see the [ox-context design doc](ox-context.md).

## Portability

The kernel's core logic has no platform dependencies beyond a dynamic memory
allocator. There is no dependency on I/O, networking, or filesystem APIs. All
external interaction is expressed as events crossing the kernel boundary.

ox-core is the composition layer that wires the kernel to concrete
implementations of Transport and Context, and provides the async executor. The
kernel itself is executor-agnostic.
