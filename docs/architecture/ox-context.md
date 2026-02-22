# ox-context: Structured Context

## Overview

ox-context is the interface between the kernel and **everything that could be in
context**. It is a structured namespace — modeled on
[StructFS](https://github.com/StructFS/structfs) — composed of **providers**
that translate heterogeneous information sources into the structured data the
kernel works with, and ultimately into concrete prompts for the LLM.

ox-context is not a prompt template or a window manager. It is the system that
makes all potentially relevant information addressable via uniform read/write
operations, and that synthesizes prompts from that information on demand.

## Relationship to the Kernel

The kernel thinks in events: StructFS-style reads and writes on paths. ox-context
is the namespace those paths resolve into. When the kernel writes an event (e.g.
a user input, a tool result), it writes to a path in ox-context. When the kernel
needs a prompt for the LLM, it reads from ox-context, which synthesizes the
prompt from all its mounted providers.

The kernel manipulates ox-context. ox-context does not drive the kernel.

## Architecture

```
            ox-context namespace
┌──────────────────────────────────────────────┐
│                                              │
│  /history/*        ──▶  History Provider     │
│                         (ox-history)         │
│                                              │
│  /system/*         ──▶  System Provider      │
│                         (base prompt,        │
│                          agent identity)     │
│                                              │
│  /tools/*          ──▶  Tool Provider        │
│                         (schemas, guidance)  │
│                                              │
│  /documents/*      ──▶  Document Provider    │
│                         (pinned references,  │
│                          project docs)       │
│                                              │
│  /session/*        ──▶  Session Provider     │
│                         (metadata, prefs)    │
│                                              │
│  /prompt           ──▶  Prompt Synthesizer   │
│                         (reads from all      │
│                          providers, produces │
│                          concrete prompt)    │
│                                              │
└──────────────────────────────────────────────┘
```

Everything is a provider. Providers are mounted at paths. The kernel reads and
writes through the namespace without knowing which provider backs a given path.

## Providers

A provider is anything that can respond to reads and writes at its mounted
path. Providers are the extension point of ox-context — adding a new kind of
contextual information means mounting a new provider.

### Built-in Providers

| Provider | Mount | Reads | Writes |
|----------|-------|-------|--------|
| **History** | `/history` | Returns the event log (conversation messages, tool calls, results). Supports sub-paths for branching and navigation. | Accepts new events (appended to the log). |
| **System** | `/system` | Returns the base system prompt and agent identity configuration. | Accepts updates to the base prompt. |
| **Tools** | `/tools` | Returns tool schemas and usage guidance, formatted for inclusion in prompts. | Accepts tool registration and deregistration. |
| **Documents** | `/documents` | Returns pinned reference material (project docs, specs, user-provided files). | Accepts document additions and removals. |
| **Session** | `/session` | Returns session metadata, user preferences, and configuration. | Accepts preference updates. |

### Custom Providers

Any information source can become a provider: a database, a web service, a
local index, a running process's state. The provider interface is deliberately
minimal — handle reads, handle writes — so that mounting new sources is
lightweight.

## Prompt Synthesis

Reading from `/prompt` triggers prompt synthesis. The synthesizer is itself a
provider, but a special one: it reads from all other providers to assemble the
concrete prompt that goes to the LLM.

Synthesis involves:

1. **Gather** — Read from each provider to collect what it contributes to the
   prompt (system instructions, conversation history, tool schemas, reference
   documents, session context).
2. **Prioritize** — Each provider-contributed block has a priority. Under token
   pressure, lower-priority blocks are dropped first. The base system prompt
   is never dropped.
3. **Window** — The conversation history (from the history provider) is
   windowed to fit the remaining token budget. Windowing strategies include
   trailing (most recent messages), pinned-prefix (keep initial messages plus
   trailing), and summarization (compress older messages).
4. **Assemble** — Combine everything into the final prompt structure: system
   prompt, message sequence, and tool definitions.

### Budget Calculation

```
model context window
  - output token reserve
  = total input budget

total input budget
  - system content (base prompt + high-priority sections)
  = message + document budget

message + document budget
  - pinned documents
  - summary block (if summarization policy)
  = verbatim message budget
```

The synthesizer fills the budget greedily: highest-priority content first, then
history newest-first, dropping or summarizing as needed.

## History as a Provider

ox-history is not a peer of ox-context — it is a provider **within** ox-context,
mounted at `/history`. The kernel writes events to `/history` (new messages,
tool results) and ox-history stores them. When the synthesizer reads from
`/history` during prompt assembly, it gets the conversation log.

ox-history's full capabilities (branching, persistence, navigation) are
accessible through sub-paths under `/history`. See the
[ox-history design doc](ox-history.md).

## Summarization

The summarization strategy within prompt synthesis requires a summarization
function. ox-context defines the interface but does not implement it — the
summary might be produced by an LLM call (routed through ox-shell), a local
heuristic, or an external service exposed as a provider.

ox-core wires the summarizer at construction time.

## Portability

The ox-context namespace, provider interface, and prompt synthesizer require
only a dynamic memory allocator. No I/O, no networking, no filesystem.
Individual providers may require platform capabilities (ox-history needs
storage for persistence, a document provider might need file access), but
ox-context itself is pure computation over the data its providers surface.
