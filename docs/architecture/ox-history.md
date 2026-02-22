# ox-history: Event Log

## Overview

ox-history is the durable event log for agent sessions. It stores the ordered
sequence of events — user inputs, assistant responses, tool calls, tool results
— that make up a conversation.

ox-history operates as a **provider within ox-context**, mounted at `/history`.
The kernel writes events to it through the ox-context namespace. The prompt
synthesizer reads from it when assembling prompts. ox-history itself is not
responsible for windowing or summarization — it stores the complete, unabridged
log. How much of that log the LLM sees is determined by ox-context's prompt
synthesis.

## Data Model

The log is a sequence of **entries**. Each entry contains:

- **ID** — Unique identifier.
- **Parent** — Pointer to the preceding entry (or none for the root).
- **Event** — The event content (user input, assistant response, tool call,
  tool result, system event).
- **Timestamp** — When the entry was created.
- **Metadata** — Turn number (which iteration of the agentic loop produced
  it), and a token count estimate (used by ox-context during prompt synthesis
  budgeting).

Parent pointers form a **tree**. Linear conversations are the degenerate case
(each entry has exactly one child). Branching occurs when the user rewinds and
re-prompts from an earlier point in the conversation.

## Operations

### Write (via ox-context at `/history`)

- Append an event as a child of the current head (normal operation).
- Append an event as a child of a specific entry (branching).

### Read (via ox-context at `/history`)

- Linear walk from root to current head (the "active branch") — this is what
  the prompt synthesizer uses.
- Linear walk from root to a specific entry (inspecting an alternate branch).
- Access all entries across all branches (for serialization or inspection).
- Count: total entries across all branches, or entries on the active branch
  only.

### Navigate

- **Checkout** — Move the head pointer to a different entry. This is how
  rewind and branch switching work.
- **Branch points** — List entries that have multiple children (fork points).
- **Children** — List the children of a given entry.

## Persistence

ox-history defines a storage interface for durable persistence with two
operations: save and load.

### Format

The canonical serialization format is **JSONL** — one JSON object per entry,
one entry per line. This is:

- **Append-friendly** — New events are appended without rewriting the file.
- **Streamable** — Can be read incrementally.
- **Human-debuggable** — Each line is a self-contained JSON object.

The tree structure is reconstructed from parent pointers on load.

### In-Memory

For ephemeral sessions (tests, one-shot scripts), the log can be created with
no backing store and lives only in memory.

## Token Estimation

Each entry carries a token estimate. ox-history does **not** tokenize — it
accepts estimates provided by the caller (typically the kernel, which gets
usage data from transport stream events). These estimates are consumed by
ox-context's prompt synthesizer when budgeting the context window.

For entries where exact counts aren't available (e.g. loaded from disk),
ox-history provides a rough heuristic: byte length divided by 4. This is
intentionally conservative and can be overridden.

## Portability

The core event log requires only a dynamic memory allocator. Storage
implementations will require platform I/O (filesystem access via standard
library or WASI), but the log itself is pure data. ox-core wires the
appropriate storage implementation based on the target platform.
