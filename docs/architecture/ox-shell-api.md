# ox-shell: StructFS API Specification

## Overview

ox-shell exposes a StructFS namespace that the kernel (and ox-context providers)
interact with through read and write operations on paths. This document
specifies the path layout, the semantics of reads and writes at each path, and
the data shapes exchanged.

All values are structured data (JSON-compatible). Reads return values; writes
accept values. Some reads are instantaneous (lookup), others return streams
(completion). The distinction is noted per-path.

### Progressive Disclosure

The namespace is structured for progressive disclosure. Collection paths return
minimal identifiers. Item paths return full details. Filtering is navigation —
drilling into a sub-path narrows the result set without loading data you don't
need.

A caller discovering models never needs to load the full catalog. It navigates:

1. `/shell/models` → list of model IDs (strings only)
2. `/shell/models/by-capability/thinking` → filtered list of IDs
3. `/shell/models/{id}` → full descriptor for one model
4. `/shell/models/{id}/capabilities` → just the capability flags

Each step returns only what was asked for. No step requires the data from any
other step.

## Namespace Layout

```
/shell
├── /models
│   ├── /active                           # read/write: current default model
│   ├── /by-provider
│   │   └── /{provider}                   # read: model IDs from this provider
│   ├── /by-capability
│   │   └── /{capability}                 # read: model IDs with this capability
│   └── /{model-id}
│       ├── /capabilities                 # read: capability flags
│       ├── /cost                         # read: pricing info
│       └── /limits                       # read: context window, max tokens
│
├── /credentials
│   └── /{provider}                       # read: status; write: set credentials
│
├── /providers
│   └── /{provider-name}
│       ├── /models                       # read: model IDs from this provider
│       └── /capabilities                 # read: provider-level capabilities
│
├── /complete                             # write: submit completion request
│   └── /{stream-id}                      # read: stream events; write: abort
│
├── /routing                              # read/write: intent → model mapping
│
└── /config
    ├── /transforms                       # read/write: message transform config
    └── /defaults                         # read/write: default completion params
```

## Path Reference

---

## Models

The model namespace is designed so that discovering the right model for a task
never requires loading the full catalog. Every collection path returns only
model IDs. Full descriptors are only loaded when you read a specific model path.

### `/shell/models`

**Read** — Returns a list of all known model IDs. Nothing else.

```
Read /shell/models

→ [
    "claude-opus-4-20250514",
    "claude-sonnet-4-5-20250514",
    "claude-haiku-4-5-20251001",
    "gpt-4o",
    "gpt-4o-mini",
    "gemini-2.5-pro"
  ]
```

**Write** — Not supported.

---

### `/shell/models/active`

**Read** — Returns the ID of the current default model.

```
Read /shell/models/active

→ "claude-sonnet-4-5-20250514"
```

**Write** — Set the default model. Accepts a model ID string or a full
descriptor (for models not in the catalog).

```
Write /shell/models/active  "claude-opus-4-20250514"

Write /shell/models/active  { "id": "llama-3.1-70b", "provider": "ollama", ... }
```

---

### `/shell/models/by-provider/{provider}`

**Read** — Returns model IDs from a specific provider.

```
Read /shell/models/by-provider/anthropic

→ ["claude-opus-4-20250514", "claude-sonnet-4-5-20250514", "claude-haiku-4-5-20251001"]

Read /shell/models/by-provider/openai

→ ["gpt-4o", "gpt-4o-mini"]
```

**Write** — Not supported.

---

### `/shell/models/by-capability/{capability}`

**Read** — Returns model IDs that support the given capability.

Defined capabilities: `thinking`, `images`, `tool_use`, `streaming`, `caching`.

```
Read /shell/models/by-capability/thinking

→ ["claude-opus-4-20250514", "claude-sonnet-4-5-20250514", "gemini-2.5-pro"]

Read /shell/models/by-capability/images

→ ["claude-opus-4-20250514", "claude-sonnet-4-5-20250514", "claude-haiku-4-5-20251001", "gpt-4o", "gemini-2.5-pro"]
```

**Write** — Not supported.

---

### `/shell/models/{model-id}`

**Read** — Returns the full descriptor for a specific model.

```
Read /shell/models/claude-sonnet-4-5-20250514

→ {
    "id": "claude-sonnet-4-5-20250514",
    "provider": "anthropic",
    "api": "anthropic-messages",
    "context_window": 200000,
    "max_output_tokens": 8192,
    "capabilities": {
      "thinking": true,
      "images": true,
      "tool_use": true,
      "streaming": true,
      "caching": true
    },
    "cost": {
      "input_per_mtok": 3.00,
      "output_per_mtok": 15.00,
      "cache_read_per_mtok": 0.30,
      "cache_write_per_mtok": 3.75
    }
  }
```

This is the only path that returns the full descriptor. All collection paths
return only IDs.

**Write** — Not supported.

---

### `/shell/models/{model-id}/capabilities`

**Read** — Returns only the capability flags.

```
Read /shell/models/claude-sonnet-4-5-20250514/capabilities

→ {
    "thinking": true,
    "images": true,
    "tool_use": true,
    "streaming": true,
    "caching": true
  }
```

---

### `/shell/models/{model-id}/cost`

**Read** — Returns only pricing information.

```
Read /shell/models/claude-sonnet-4-5-20250514/cost

→ {
    "input_per_mtok": 3.00,
    "output_per_mtok": 15.00,
    "cache_read_per_mtok": 0.30,
    "cache_write_per_mtok": 3.75
  }
```

---

### `/shell/models/{model-id}/limits`

**Read** — Returns only size limits.

```
Read /shell/models/claude-sonnet-4-5-20250514/limits

→ {
    "context_window": 200000,
    "max_output_tokens": 8192
  }
```

---

## Credentials

### `/shell/credentials/{provider}`

**Read** — Returns credential status only. Credential values are never returned
via read — they are write-only for security.

```
Read /shell/credentials/anthropic

→ { "status": "configured", "type": "api_key" }

Read /shell/credentials/google

→ { "status": "missing" }
```

**Write** — Set credentials for a provider.

```
Write /shell/credentials/anthropic  { "type": "api_key", "value": "sk-ant-..." }

Write /shell/credentials/openai  { "type": "bearer", "value": "..." }
```

Credentials are held in memory only. The shell does not persist them. The
credential store is populated at construction (from the host environment) and
can be updated at runtime via writes.

---

## Providers

### `/shell/providers`

**Read** — Returns a list of registered provider names.

```
Read /shell/providers

→ ["anthropic", "openai", "ollama"]
```

---

### `/shell/providers/{provider-name}`

**Read** — Returns provider status and metadata.

```
Read /shell/providers/anthropic

→ {
    "name": "anthropic",
    "api": "anthropic-messages",
    "status": "ready",
    "base_url": "https://api.anthropic.com"
  }
```

Status values: `ready` (credentials present, provider functional),
`no_credentials`, `error`.

---

### `/shell/providers/{provider-name}/models`

**Read** — Returns model IDs from this provider. Equivalent to
`/shell/models/by-provider/{provider}` (same data, different navigation path).

```
Read /shell/providers/anthropic/models

→ ["claude-opus-4-20250514", "claude-sonnet-4-5-20250514", "claude-haiku-4-5-20251001"]
```

---

### `/shell/providers/{provider-name}/capabilities`

**Read** — Returns provider-level capabilities (what the API protocol
supports, independent of any specific model).

```
Read /shell/providers/anthropic/capabilities

→ {
    "streaming": true,
    "thinking": true,
    "tool_use": true,
    "images": true,
    "caching": true
  }
```

---

## Completions

### `/shell/complete`

The primary interaction point. The kernel writes a completion request and
receives a stream handle for reading back events.

**Write** — Submit a completion request. Returns a stream handle.

```
Write /shell/complete
{
  "system_prompt": "You are a helpful assistant.",
  "messages": [
    { "role": "user", "content": "Hello" },
    { "role": "assistant", "content": [{ "type": "text", "text": "Hi!" }] },
    { "role": "user", "content": "What is 2+2?" }
  ],
  "tools": [
    {
      "name": "calculator",
      "description": "Evaluate a math expression",
      "parameters": { "type": "object", "properties": { ... } }
    }
  ],
  "config": {
    "max_tokens": 4096,
    "temperature": 0.7,
    "thinking": "medium"
  },
  "routing": {
    "model": "claude-sonnet-4-5-20250514",
    "intent": "execute"
  }
}

→ { "stream_id": "s-001" }
```

The `routing` field controls per-request model selection:

- **`routing.model`** — Explicit model ID. Overrides the active model.
- **`routing.intent`** — Semantic hint. Resolved via the routing table.

Both are optional. If neither is present, the request uses the active model.

**Read** — Not supported at this path. Read from the stream handle.

---

### `/shell/complete/{stream-id}`

**Read** — Returns the next event from a completion stream. Repeated reads
drain the stream. The stream terminates with a `done` or `error` event.

```
Read /shell/complete/s-001  →  { "type": "start" }
Read /shell/complete/s-001  →  { "type": "text_delta", "content_index": 0, "delta": "The answer" }
Read /shell/complete/s-001  →  { "type": "text_delta", "content_index": 0, "delta": " is 4." }
Read /shell/complete/s-001  →  { "type": "done", "stop_reason": "end_turn", "usage": { ... }, "model": "..." }
```

After a terminal event, further reads return an error (stream exhausted).

**Write** — Abort a running stream.

```
Write /shell/complete/s-001  { "abort": true }
```

---

## Routing

### `/shell/routing`

**Read** — Returns the intent-to-model routing table.

```
Read /shell/routing

→ {
    "plan": "claude-haiku-4-5-20251001",
    "execute": "claude-sonnet-4-5-20250514",
    "reflect": "claude-sonnet-4-5-20250514",
    "summarize": "claude-haiku-4-5-20251001",
    "recover": "claude-sonnet-4-5-20250514",
    "default": "claude-sonnet-4-5-20250514"
  }
```

**Write** — Update routing entries. Partial writes merge; unmentioned keys are
unchanged.

```
Write /shell/routing  { "plan": "claude-haiku-4-5-20251001", "summarize": "gpt-4o-mini" }
```

### Defined Intents

| Intent | Semantics |
|--------|-----------|
| `plan` | Deciding what to do next. Favors speed and cost efficiency. |
| `execute` | Generating the primary output. Favors capability. |
| `reflect` | Evaluating results, checking correctness. Favors reasoning. |
| `summarize` | Compressing information. Favors throughput and cost. |
| `recover` | Handling errors, retrying. Favors reasoning. |

### Resolution Order

When the shell receives a completion request, it resolves the model in this
order:

1. `routing.model` if present → use that model
2. `routing.intent` if present → look up in routing table at `/shell/routing`
3. `default` entry in routing table
4. `/shell/models/active`

If both `model` and `intent` are present, `model` wins. This allows the kernel
to override policy for a specific request while still expressing intent for
observability.

---

## Configuration

### `/shell/config/transforms`

**Read** — Returns the message transformation pipeline configuration.

```
Read /shell/config/transforms

→ {
    "merge_consecutive": true,
    "strip_empty_blocks": true,
    "strip_trailing_errors": true,
    "normalize_tool_ids": true,
    "batch_tool_results": true,
    "filter_images_for_text_models": true
  }
```

**Write** — Override settings. Partial writes merge.

---

### `/shell/config/defaults`

**Read** — Returns default completion parameters applied to every request
before per-request overrides.

```
Read /shell/config/defaults

→ {
    "max_tokens": 4096,
    "temperature": 1.0,
    "thinking": "off"
  }
```

**Write** — Update defaults. Partial writes merge.

---

## Stream Event Reference

Events returned when reading from `/shell/complete/{stream-id}`:

### Content Events

| Event | Fields | Description |
|-------|--------|-------------|
| `start` | — | Stream has begun. Always first. |
| `text_start` | `content_index` | A text block is beginning. |
| `text_delta` | `content_index`, `delta` | Incremental text. |
| `text_end` | `content_index` | Text block complete. |
| `thinking_start` | `content_index` | A reasoning block is beginning. |
| `thinking_delta` | `content_index`, `delta` | Incremental thinking. |
| `thinking_end` | `content_index` | Thinking block complete. |
| `tool_call_start` | `content_index`, `id`, `name` | A tool call is beginning. |
| `tool_call_delta` | `content_index`, `id`, `arguments_fragment` | Incremental argument JSON. |
| `tool_call_end` | `content_index`, `id` | Tool call complete. Arguments parseable. |

### Terminal Events

| Event | Fields | Description |
|-------|--------|-------------|
| `done` | `stop_reason`, `usage`, `model` | Stream complete. `model` confirms which model served the request. |
| `error` | `code`, `message`, `retryable` | Stream failed. No further events. |

Stop reasons: `end_turn`, `tool_use`, `max_tokens`.

Error codes: `rate_limited`, `auth_failed`, `model_not_found`,
`provider_error`, `timeout`, `aborted`, `internal`.

---

## Error Handling

Errors from any path operation are returned as structured values:

```
{
  "error": true,
  "code": "not_found",
  "path": "/shell/models/nonexistent",
  "message": "Model not found in catalog"
}
```

| Code | Meaning |
|------|---------|
| `not_found` | Path does not exist or resource not found |
| `not_writable` | Write attempted on a read-only path |
| `not_readable` | Read attempted on a write-only path |
| `invalid_value` | Written value does not match expected shape |
| `no_credentials` | Completion requested but provider has no credentials |
| `provider_error` | LLM API returned an error |
| `rate_limited` | LLM API rate limit hit |
| `auth_failed` | Credentials rejected by provider |
| `stream_exhausted` | Read from a completed stream |
| `internal` | Unexpected error |

---

## Discovery Walkthrough

A kernel that needs to pick a model for a planning step, favoring cost and
speed, without ever loading the full catalog:

```
Read /shell/models/by-capability/tool_use
→ ["claude-opus-4-20250514", "claude-sonnet-4-5-20250514", "claude-haiku-4-5-20251001", "gpt-4o", "gpt-4o-mini"]

Read /shell/models/claude-haiku-4-5-20251001/cost
→ { "input_per_mtok": 0.80, "output_per_mtok": 4.00, ... }

Read /shell/models/gpt-4o-mini/cost
→ { "input_per_mtok": 0.15, "output_per_mtok": 0.60, ... }

Read /shell/models/gpt-4o-mini/limits
→ { "context_window": 128000, "max_output_tokens": 16384 }
```

Four reads, each returning a small value. The kernel now knows enough to pick
gpt-4o-mini for cheap planning without ever loading descriptors for models it
doesn't care about.

Alternatively, if routing policy is already configured:

```
Write /shell/complete  { ..., "routing": { "intent": "plan" } }
```

One write. The shell resolves the intent to the configured model. Zero
discovery reads needed.
