# Complete Tool with Ref Resolution

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace hardcoded prompt synthesis with a `complete` function that resolves typed context references, making `CompletionRequest` an internal detail and exposing `complete` as an LLM-callable tool.

**Architecture:** `ContextRef` is a tagged enum (system, history, tools, raw) that describes what context to include in a completion. `resolve_refs` reads from the namespace and returns `ResolvedContext` — the raw materials for a prompt. `complete` resolves refs, assembles the request internally, and sends it via the existing `tools/completions/complete/{account}` transport. `run_turn` constructs bootstrap refs and calls `complete`. The LLM sees `complete` in `tools/schemas` and can call it — the kernel special-cases it in `execute_tools` by calling the `complete` function directly (no borrow problem since it has `&mut dyn Store`).

**Tech Stack:** Rust (edition 2024), StructFS, ox-kernel, ox-tools, ox-core.

---

## Scope Check

Single subsystem: the `complete` tool. This plan does NOT cover:
- Inner loop (recursive tool execution inside `complete`) — the tool fires one completion and returns
- Handle registry / async execution
- History as log projection
- Multi-completion turns

These are follow-up work. This plan makes `complete` callable, ref-based, and LLM-visible.

---

## File Structure

### Modified files

| File | Change |
|------|--------|
| `crates/ox-kernel/src/run.rs` | Add `ContextRef` enum, `ResolvedContext` struct, `resolve_refs()`, `complete()`, `default_refs()`. Refactor `run_turn` to use `complete`. Refactor `execute_tools` to special-case "complete". Keep `synthesize` as a thin wrapper (ox-web compat). |
| `crates/ox-kernel/src/lib.rs` | Re-export new public types: `ContextRef`, `ResolvedContext`, `resolve_refs`, `complete`, `default_refs`. |
| `crates/ox-tools/src/completion.rs` | Add `complete` to `CompletionModule::schemas()` return value. |
| `crates/ox-core/src/lib.rs` | Add integration test for `complete` tool call via the LLM. Update re-exports. |

---

## Subsystem 1: Ref Types and Resolution

### Task 1: ContextRef, ResolvedContext, resolve_refs

**Files:**
- Modify: `crates/ox-kernel/src/run.rs`

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` block in `crates/ox-kernel/src/run.rs`:

```rust
#[test]
fn resolve_system_ref() {
    let mut store = MockStore::new();
    store.set("system", Value::String("You are helpful.".into()));
    let refs = vec![ContextRef::System {
        path: "system".into(),
    }];
    let resolved = resolve_refs(&mut store, &refs).unwrap();
    assert_eq!(resolved.system, "You are helpful.");
}

#[test]
fn resolve_history_ref() {
    let mut store = MockStore::new();
    store.set(
        "history/messages",
        structfs_serde_store::json_to_value(serde_json::json!([
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": [{"type": "text", "text": "b"}]},
            {"role": "user", "content": "c"},
        ])),
    );
    let refs = vec![ContextRef::History {
        path: "history/messages".into(),
        last: Some(2),
    }];
    let resolved = resolve_refs(&mut store, &refs).unwrap();
    assert_eq!(resolved.messages.len(), 2);
}

#[test]
fn resolve_history_ref_no_window() {
    let mut store = MockStore::new();
    store.set(
        "history/messages",
        structfs_serde_store::json_to_value(serde_json::json!([
            {"role": "user", "content": "a"},
        ])),
    );
    let refs = vec![ContextRef::History {
        path: "history/messages".into(),
        last: None,
    }];
    let resolved = resolve_refs(&mut store, &refs).unwrap();
    assert_eq!(resolved.messages.len(), 1);
}

#[test]
fn resolve_tools_ref_with_only_filter() {
    let mut store = MockStore::new();
    store.set(
        "tools/schemas",
        structfs_serde_store::json_to_value(serde_json::json!([
            {"name": "read_file", "description": "read", "input_schema": {}},
            {"name": "shell", "description": "run", "input_schema": {}},
            {"name": "write_file", "description": "write", "input_schema": {}},
        ])),
    );
    let refs = vec![ContextRef::Tools {
        path: "tools/schemas".into(),
        only: Some(vec!["read_file".into(), "shell".into()]),
        except: None,
    }];
    let resolved = resolve_refs(&mut store, &refs).unwrap();
    assert_eq!(resolved.tools.len(), 2);
    assert!(resolved.tools.iter().any(|t| t.name == "read_file"));
    assert!(resolved.tools.iter().any(|t| t.name == "shell"));
}

#[test]
fn resolve_tools_ref_with_except_filter() {
    let mut store = MockStore::new();
    store.set(
        "tools/schemas",
        structfs_serde_store::json_to_value(serde_json::json!([
            {"name": "read_file", "description": "read", "input_schema": {}},
            {"name": "shell", "description": "run", "input_schema": {}},
        ])),
    );
    let refs = vec![ContextRef::Tools {
        path: "tools/schemas".into(),
        only: None,
        except: Some(vec!["shell".into()]),
    }];
    let resolved = resolve_refs(&mut store, &refs).unwrap();
    assert_eq!(resolved.tools.len(), 1);
    assert_eq!(resolved.tools[0].name, "read_file");
}

#[test]
fn resolve_raw_ref() {
    let mut store = MockStore::new();
    let refs = vec![ContextRef::Raw {
        content: "Extra instructions here.".into(),
    }];
    let resolved = resolve_refs(&mut store, &refs).unwrap();
    assert_eq!(resolved.extra_content, vec!["Extra instructions here."]);
}

#[test]
fn resolve_multiple_refs() {
    let mut store = MockStore::new();
    store.set("system", Value::String("sys".into()));
    store.set(
        "history/messages",
        structfs_serde_store::json_to_value(serde_json::json!([])),
    );
    store.set(
        "tools/schemas",
        structfs_serde_store::json_to_value(serde_json::json!([])),
    );
    let refs = vec![
        ContextRef::System {
            path: "system".into(),
        },
        ContextRef::History {
            path: "history/messages".into(),
            last: None,
        },
        ContextRef::Tools {
            path: "tools/schemas".into(),
            only: None,
            except: None,
        },
        ContextRef::Raw {
            content: "bonus".into(),
        },
    ];
    let resolved = resolve_refs(&mut store, &refs).unwrap();
    assert_eq!(resolved.system, "sys");
    assert!(resolved.messages.is_empty());
    assert!(resolved.tools.is_empty());
    assert_eq!(resolved.extra_content, vec!["bonus"]);
}

#[test]
fn context_ref_serde_roundtrip() {
    let refs = vec![
        ContextRef::System {
            path: "system".into(),
        },
        ContextRef::History {
            path: "history/messages".into(),
            last: Some(20),
        },
        ContextRef::Tools {
            path: "tools/schemas".into(),
            only: Some(vec!["read_file".into()]),
            except: None,
        },
        ContextRef::Raw {
            content: "hello".into(),
        },
    ];
    let json = serde_json::to_value(&refs).unwrap();
    let roundtripped: Vec<ContextRef> = serde_json::from_value(json).unwrap();
    assert_eq!(roundtripped.len(), 4);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-kernel -- run::tests::resolve -v`
Expected: FAIL — `ContextRef` not found.

- [ ] **Step 3: Write the types and resolve_refs implementation**

Add to `crates/ox-kernel/src/run.rs`, after the imports and before the stream event codec section:

```rust
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Context references
// ---------------------------------------------------------------------------

/// A typed reference to context that should be included in a completion.
///
/// The `complete` function resolves these by reading from the namespace.
/// The LLM sees the schema and can construct refs when calling `complete`
/// as a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContextRef {
    /// Read a system prompt string from the given path.
    #[serde(rename = "system")]
    System { path: String },

    /// Read conversation messages from the given path, optionally windowed.
    #[serde(rename = "history")]
    History {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        last: Option<usize>,
    },

    /// Read tool schemas from the given path, optionally filtered.
    #[serde(rename = "tools")]
    Tools {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        only: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        except: Option<Vec<String>>,
    },

    /// Literal string content, included as-is.
    #[serde(rename = "raw")]
    Raw { content: String },
}

/// The result of resolving a set of context references.
#[derive(Debug, Clone, Default)]
pub struct ResolvedContext {
    /// System prompt string (from the last System ref resolved).
    pub system: String,
    /// Conversation messages in wire format.
    pub messages: Vec<serde_json::Value>,
    /// Tool schemas (filtered if the ref specified only/except).
    pub tools: Vec<ToolSchema>,
    /// Extra content from Raw refs, appended to system prompt.
    pub extra_content: Vec<String>,
}

/// Resolve context references by reading from the namespace.
///
/// Each ref reads from its specified path. Refs that fail produce errors
/// (callers should construct refs that point to valid paths).
pub fn resolve_refs(
    context: &mut dyn Reader,
    refs: &[ContextRef],
) -> Result<ResolvedContext, String> {
    let mut resolved = ResolvedContext::default();

    for r in refs {
        match r {
            ContextRef::System { path } => {
                let p = Path::parse(path).map_err(|e| e.to_string())?;
                match context.read(&p).map_err(|e| e.to_string())? {
                    Some(Record::Parsed(Value::String(s))) => resolved.system = s,
                    Some(_) => return Err(format!("expected string at {path}")),
                    None => return Err(format!("nothing at {path}")),
                }
            }
            ContextRef::History { path, last } => {
                let p = Path::parse(path).map_err(|e| e.to_string())?;
                let json = match context.read(&p).map_err(|e| e.to_string())? {
                    Some(Record::Parsed(v)) => structfs_serde_store::value_to_json(v),
                    _ => return Err(format!("expected parsed record at {path}")),
                };
                let mut messages: Vec<serde_json::Value> =
                    serde_json::from_value(json).map_err(|e| e.to_string())?;
                if let Some(n) = last {
                    let start = messages.len().saturating_sub(*n);
                    messages = messages[start..].to_vec();
                }
                resolved.messages = messages;
            }
            ContextRef::Tools { path, only, except } => {
                let p = Path::parse(path).map_err(|e| e.to_string())?;
                let json = match context.read(&p).map_err(|e| e.to_string())? {
                    Some(Record::Parsed(v)) => structfs_serde_store::value_to_json(v),
                    _ => return Err(format!("expected parsed record at {path}")),
                };
                let mut tools: Vec<ToolSchema> =
                    serde_json::from_value(json).map_err(|e| e.to_string())?;
                if let Some(only) = only {
                    tools.retain(|t| only.contains(&t.name));
                }
                if let Some(except) = except {
                    tools.retain(|t| !except.contains(&t.name));
                }
                resolved.tools = tools;
            }
            ContextRef::Raw { content } => {
                resolved.extra_content.push(content.clone());
            }
        }
    }

    Ok(resolved)
}

/// The default bootstrap refs: system prompt, full history, all tools.
pub fn default_refs() -> Vec<ContextRef> {
    vec![
        ContextRef::System {
            path: "system".into(),
        },
        ContextRef::History {
            path: "history/messages".into(),
            last: None,
        },
        ContextRef::Tools {
            path: "tools/schemas".into(),
            only: None,
            except: None,
        },
    ]
}
```

Note: add `use serde::{Deserialize, Serialize};` to the top of run.rs if not already imported. It's already available through the crate since `crate::ToolSchema` uses serde, but the import may need to be explicit for the derive macros.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-kernel -- run::tests -v`
Expected: PASS — all new and existing tests.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-kernel/src/run.rs
git commit -m "feat(ox-kernel): add ContextRef types, resolve_refs, and default_refs"
```

---

### Task 2: `complete` function and `run_turn` refactor

**Files:**
- Modify: `crates/ox-kernel/src/run.rs`
- Modify: `crates/ox-kernel/src/lib.rs` (update re-exports)

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` block:

```rust
#[test]
fn complete_resolves_refs_and_sends() {
    let mut store = MockStore::new();
    store.set("system", Value::String("You are helpful.".into()));
    store.set(
        "history/messages",
        structfs_serde_store::json_to_value(serde_json::json!([
            {"role": "user", "content": "hi"}
        ])),
    );
    store.set(
        "tools/schemas",
        structfs_serde_store::json_to_value(serde_json::json!([])),
    );
    store.set("gate/defaults/model", Value::String("test-model".into()));
    store.set("gate/defaults/max_tokens", Value::Integer(100));
    store.push_completion_response(serde_json::json!([
        {"type": "text_delta", "text": "Hello!"},
        {"type": "message_stop"}
    ]));

    let refs = default_refs();
    let events = complete(&mut store, "test", &refs).unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hello!"));
}

#[test]
fn complete_with_raw_ref_appends_to_system() {
    let mut store = MockStore::new();
    store.set("system", Value::String("Base prompt.".into()));
    store.set(
        "history/messages",
        structfs_serde_store::json_to_value(serde_json::json!([])),
    );
    store.set(
        "tools/schemas",
        structfs_serde_store::json_to_value(serde_json::json!([])),
    );
    store.set("gate/defaults/model", Value::String("test".into()));
    store.set("gate/defaults/max_tokens", Value::Integer(100));
    store.push_completion_response(serde_json::json!([
        {"type": "text_delta", "text": "ok"},
        {"type": "message_stop"}
    ]));

    let refs = vec![
        ContextRef::System {
            path: "system".into(),
        },
        ContextRef::History {
            path: "history/messages".into(),
            last: None,
        },
        ContextRef::Tools {
            path: "tools/schemas".into(),
            only: None,
            except: None,
        },
        ContextRef::Raw {
            content: "Extra instruction.".into(),
        },
    ];
    let events = complete(&mut store, "test", &refs).unwrap();
    assert!(!events.is_empty());

    // Verify the written request included extra content in system
    let written = store.appended.iter().find(|(p, _)| p.contains("completions/complete"));
    assert!(written.is_some(), "completion request should have been written");
    let request_json = structfs_serde_store::value_to_json(written.unwrap().1.clone());
    let system = request_json.get("system").and_then(|v| v.as_str()).unwrap();
    assert!(system.contains("Base prompt."));
    assert!(system.contains("Extra instruction."));
}

#[test]
fn complete_with_history_windowing() {
    let mut store = MockStore::new();
    store.set("system", Value::String("sys".into()));
    store.set(
        "history/messages",
        structfs_serde_store::json_to_value(serde_json::json!([
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": [{"type": "text", "text": "b"}]},
            {"role": "user", "content": "c"},
        ])),
    );
    store.set(
        "tools/schemas",
        structfs_serde_store::json_to_value(serde_json::json!([])),
    );
    store.set("gate/defaults/model", Value::String("test".into()));
    store.set("gate/defaults/max_tokens", Value::Integer(100));
    store.push_completion_response(serde_json::json!([
        {"type": "text_delta", "text": "ok"},
        {"type": "message_stop"}
    ]));

    let refs = vec![
        ContextRef::System {
            path: "system".into(),
        },
        ContextRef::History {
            path: "history/messages".into(),
            last: Some(1),
        },
        ContextRef::Tools {
            path: "tools/schemas".into(),
            only: None,
            except: None,
        },
    ];
    let events = complete(&mut store, "test", &refs).unwrap();
    assert!(!events.is_empty());

    // Verify only 1 message was sent
    let written = store.appended.iter().find(|(p, _)| p.contains("completions/complete"));
    let request_json = structfs_serde_store::value_to_json(written.unwrap().1.clone());
    let messages = request_json.get("messages").and_then(|v| v.as_array()).unwrap();
    assert_eq!(messages.len(), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-kernel -- run::tests::complete -v`
Expected: FAIL — `complete` function not found.

- [ ] **Step 3: Write the `complete` function**

Add to `crates/ox-kernel/src/run.rs`, in the "Building blocks" section (after `resolve_refs`, before `accumulate_response`):

```rust
/// Fire a completion with resolved context references.
///
/// 1. Resolves refs by reading from the namespace
/// 2. Reads model config from `gate/defaults/`
/// 3. Assembles a CompletionRequest (internal detail)
/// 4. Sends via `tools/completions/complete/{account}`
/// 5. Returns stream events
///
/// This is the kernel's completion primitive. `run_turn` calls it with
/// bootstrap refs. The LLM can call it as a tool (with custom refs).
pub fn complete(
    context: &mut dyn Store,
    account: &str,
    refs: &[ContextRef],
) -> Result<Vec<StreamEvent>, String> {
    let resolved = resolve_refs(context, refs)?;

    // Read model configuration
    let (model, max_tokens) = read_model_config(context)?;

    // Assemble system prompt (base + any raw content)
    let system = if resolved.extra_content.is_empty() {
        resolved.system
    } else {
        format!(
            "{}\n\n{}",
            resolved.system,
            resolved.extra_content.join("\n\n")
        )
    };

    let request = CompletionRequest {
        model,
        max_tokens,
        system,
        messages: resolved.messages,
        tools: resolved.tools,
        stream: true,
    };

    send_completion(context, account, &request)
}
```

Add the `read_model_config` helper in the internal helpers section:

```rust
/// Read model ID and max_tokens from gate defaults.
fn read_model_config(context: &mut dyn Reader) -> Result<(String, u32), String> {
    let model = match context
        .read(&path!("gate/defaults/model"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(Value::String(s))) => s,
        _ => return Err("expected string from gate/defaults/model".into()),
    };
    let max_tokens = match context
        .read(&path!("gate/defaults/max_tokens"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(Value::Integer(n))) => n as u32,
        _ => return Err("expected integer from gate/defaults/max_tokens".into()),
    };
    Ok((model, max_tokens))
}
```

- [ ] **Step 4: Refactor `synthesize` to delegate to `resolve_refs`**

Replace the current `synthesize` body with:

```rust
/// Read prompt components from context and assemble a [`CompletionRequest`].
///
/// This is a convenience wrapper for async consumers (like ox-web) that
/// need the CompletionRequest for their own transport. Sync consumers
/// should prefer [`complete`] which handles transport internally.
pub fn synthesize(context: &mut dyn Reader) -> Result<CompletionRequest, String> {
    let refs = default_refs();
    let resolved = resolve_refs(context, &refs)?;
    let (model, max_tokens) = read_model_config(context)?;

    Ok(CompletionRequest {
        model,
        max_tokens,
        system: resolved.system,
        messages: resolved.messages,
        tools: resolved.tools,
        stream: true,
    })
}
```

- [ ] **Step 5: Refactor `run_turn` to use `complete`**

Replace the `run_turn` function body:

```rust
pub fn run_turn(context: &mut dyn Store, emit: &mut dyn FnMut(AgentEvent)) -> Result<(), String> {
    let account = read_default_account(context)?;
    let refs = default_refs();

    loop {
        emit(AgentEvent::TurnStart);

        let events = complete(context, &account, &refs)?;
        let content = accumulate_response(events, emit)?;

        // Log assistant entry
        log_entry(
            context,
            serde_json::json!({
                "type": "assistant",
                "content": serde_json::to_value(&content).unwrap_or(serde_json::Value::Null),
                "source": { "account": &account }
            }),
        );

        let tool_calls = record_turn(context, &content)?;

        if tool_calls.is_empty() {
            emit(AgentEvent::TurnEnd);
            return Ok(());
        }

        // Log tool calls
        for tc in &tool_calls {
            log_entry(
                context,
                serde_json::json!({
                    "type": "tool_call",
                    "id": tc.id,
                    "name": tc.name,
                    "input": tc.input,
                }),
            );
        }

        let results = execute_tools(context, &tool_calls, emit)?;

        // Log tool results
        for r in &results {
            log_entry(
                context,
                serde_json::json!({
                    "type": "tool_result",
                    "id": r.tool_use_id,
                    "output": r.content,
                }),
            );
        }

        record_tool_results(context, &results)?;
    }
}
```

- [ ] **Step 6: Update re-exports in `crates/ox-kernel/src/lib.rs`**

Add `ContextRef`, `ResolvedContext`, `resolve_refs`, `complete`, `default_refs` to the `pub use run::{...}` block.

- [ ] **Step 7: Run all tests**

Run: `cargo test -p ox-kernel -v && cargo check --workspace`
Expected: PASS — all existing tests still pass (synthesize tests use the same paths, run_turn tests work through complete now).

- [ ] **Step 8: Commit**

```bash
git add crates/ox-kernel/src/run.rs crates/ox-kernel/src/lib.rs
git commit -m "feat(ox-kernel): add complete function with ref resolution, refactor run_turn"
```

---

## Subsystem 2: LLM-Callable Complete Tool

### Task 3: Register `complete` in tool schemas

**Files:**
- Modify: `crates/ox-tools/src/completion.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing tests in `crates/ox-tools/src/completion.rs`:

```rust
#[test]
fn schemas_includes_complete_tool() {
    let gate = GateStore::new();
    let module = CompletionModule::new(gate);
    let schemas = module.schemas();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].wire_name, "complete");
    let input = &schemas[0].input_schema;
    assert!(input.get("properties").unwrap().get("account").is_some());
    assert!(input.get("properties").unwrap().get("refs").is_some());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- completion::tests::schemas_includes -v`
Expected: FAIL — schemas() returns empty vec.

- [ ] **Step 3: Update `CompletionModule::schemas` to return the complete tool schema**

In `crates/ox-tools/src/completion.rs`, replace the `schemas` method:

```rust
/// Tool schemas for the completion module.
///
/// Exposes `complete` — an LLM-callable tool that fires a completion
/// with specified context references.
pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
    vec![ToolSchemaEntry {
        wire_name: "complete".to_string(),
        internal_path: "completions/complete".to_string(),
        description: "Fire an LLM completion with specified context references. \
            Use this to delegate sub-tasks to a model with custom context."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "account": {
                    "type": "string",
                    "description": "Account name for the completion (e.g. 'anthropic', 'openai')"
                },
                "refs": {
                    "type": "array",
                    "description": "Context references to include in the prompt",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": {
                                "type": "string",
                                "enum": ["system", "history", "tools", "raw"],
                                "description": "Reference type"
                            },
                            "path": {
                                "type": "string",
                                "description": "Namespace path to read from (for system, history, tools)"
                            },
                            "last": {
                                "type": "integer",
                                "description": "For history: only include the last N messages"
                            },
                            "only": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "For tools: only include these tool names"
                            },
                            "except": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "For tools: exclude these tool names"
                            },
                            "content": {
                                "type": "string",
                                "description": "For raw: literal content to include"
                            }
                        },
                        "required": ["type"]
                    }
                }
            },
            "required": ["account", "refs"]
        }),
    }]
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools -- completion::tests -v`
Expected: PASS — including the new test.

Note: the existing `schemas_returns_empty_without_keys` test will now fail. Update it:
```rust
#[test]
fn schemas_returns_complete_tool() {
    let gate = GateStore::new();
    let module = CompletionModule::new(gate);
    assert_eq!(module.schemas().len(), 1);
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/completion.rs
git commit -m "feat(ox-tools): register complete tool in CompletionModule schemas"
```

---

### Task 4: Kernel special-cases `complete` in `execute_tools`

**Files:**
- Modify: `crates/ox-kernel/src/run.rs`

When the LLM calls `complete` as a tool, the kernel handles it by calling the `complete` free function directly — NOT by routing through the ToolStore (which would cause borrow issues). The tool result is the final text from the completion.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[test]
fn execute_tools_handles_complete_tool_call() {
    let mut store = MockStore::new();
    // Set up context for the inner complete call
    store.set("system", Value::String("Inner prompt.".into()));
    store.set(
        "history/messages",
        structfs_serde_store::json_to_value(serde_json::json!([])),
    );
    store.set(
        "tools/schemas",
        structfs_serde_store::json_to_value(serde_json::json!([])),
    );
    store.set("gate/defaults/model", Value::String("test".into()));
    store.set("gate/defaults/max_tokens", Value::Integer(100));
    store.push_completion_response(serde_json::json!([
        {"type": "text_delta", "text": "Inner response."},
        {"type": "message_stop"}
    ]));

    let tool_calls = vec![ToolCall {
        id: "tc1".into(),
        name: "complete".into(),
        input: serde_json::json!({
            "account": "test",
            "refs": [
                {"type": "system", "path": "system"},
                {"type": "history", "path": "history/messages"},
                {"type": "tools", "path": "tools/schemas"}
            ]
        }),
    }];

    let mut events = vec![];
    let results =
        execute_tools(&mut store, &tool_calls, &mut |e| events.push(format!("{e:?}")))
            .unwrap();
    assert_eq!(results.len(), 1);
    let content = results[0].content.as_str().unwrap();
    assert!(content.contains("Inner response."));
    assert!(events.iter().any(|e| e.contains("ToolCallStart")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-kernel -- run::tests::execute_tools_handles_complete -v`
Expected: FAIL — current execute_tools tries to write to `tools/complete` which doesn't work with MockStore.

- [ ] **Step 3: Refactor `execute_tools` to special-case `complete`**

Replace the `execute_tools` function:

```rust
/// Execute tool calls, returning results.
///
/// The `complete` tool is handled specially — the kernel calls the
/// [`complete`] function directly with the context, avoiding the
/// ToolStore's borrow chain. All other tools route through the store.
pub fn execute_tools(
    context: &mut dyn Store,
    tool_calls: &[ToolCall],
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ToolResult>, String> {
    let mut results = Vec::new();

    for tc in tool_calls {
        emit(AgentEvent::ToolCallStart {
            name: tc.name.clone(),
        });

        let result_str = if tc.name == "complete" {
            execute_complete_tool(context, &tc.input)?
        } else {
            execute_normal_tool(context, tc)?
        };

        emit(AgentEvent::ToolCallResult {
            name: tc.name.clone(),
            result: result_str.clone(),
        });

        results.push(ToolResult {
            tool_use_id: tc.id.clone(),
            content: serde_json::Value::String(result_str),
        });
    }

    Ok(results)
}

/// Handle the `complete` tool: parse refs from input, call `complete`, return text.
fn execute_complete_tool(
    context: &mut dyn Store,
    input: &serde_json::Value,
) -> Result<String, String> {
    let account = input
        .get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("anthropic");
    let refs_value = input
        .get("refs")
        .ok_or("complete tool: missing 'refs' field")?;
    let refs: Vec<ContextRef> =
        serde_json::from_value(refs_value.clone()).map_err(|e| format!("invalid refs: {e}"))?;

    let events = complete(context, account, &refs)?;
    let content = accumulate_response(events, &mut |_| {})?;

    // Extract text from the response
    let text = content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    Ok(text)
}

/// Execute a normal (non-complete) tool through the store.
fn execute_normal_tool(context: &mut dyn Store, tc: &ToolCall) -> Result<String, String> {
    let input_value = structfs_serde_store::json_to_value(tc.input.clone());
    let tool_path = Path::parse(&format!("tools/{}", tc.name)).map_err(|e| e.to_string())?;

    match context.write(&tool_path, Record::parsed(input_value)) {
        Ok(_) => {
            let result_path =
                Path::parse(&format!("tools/{}/result", tc.name)).map_err(|e| e.to_string())?;
            match context.read(&result_path) {
                Ok(Some(record)) => {
                    let val = record.as_value().cloned().unwrap_or(Value::Null);
                    let json = structfs_serde_store::value_to_json(val);
                    Ok(serde_json::to_string(&json).unwrap_or_default())
                }
                Ok(None) => Ok(format!("error: no result for tool {}", tc.name)),
                Err(e) => Ok(format!("error: {e}")),
            }
        }
        Err(e) => Ok(e.to_string()),
    }
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test -p ox-kernel -- run::tests -v && cargo test --workspace`
Expected: PASS — all tests including existing execute_tools tests.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-kernel/src/run.rs
git commit -m "feat(ox-kernel): execute_tools special-cases complete tool, calls complete() directly"
```

---

### Task 5: Integration test + final wiring

**Files:**
- Modify: `crates/ox-core/src/lib.rs` (add integration test, update re-exports)

- [ ] **Step 1: Write integration test for LLM calling complete as a tool**

Add to the existing `mod tests` block in `crates/ox-core/src/lib.rs`:

```rust
#[test]
fn run_turn_llm_calls_complete_tool() {
    // Simulate: LLM's first response is a "complete" tool call.
    // The kernel should handle it by calling complete() directly,
    // which fires a sub-completion. Then the LLM's second response
    // uses the sub-completion result.
    let transport = SequentialTransport::new(vec![
        // Outer completion: LLM calls the complete tool
        (
            vec![
                StreamEvent::ToolUseStart {
                    id: "tc1".into(),
                    name: "complete".into(),
                },
                StreamEvent::ToolUseInputDelta(
                    serde_json::json!({
                        "account": "anthropic",
                        "refs": [
                            {"type": "system", "path": "system"},
                            {"type": "raw", "content": "Summarize briefly."}
                        ]
                    })
                    .to_string(),
                ),
                StreamEvent::MessageStop,
            ],
            10,
            5,
        ),
        // Inner completion (fired by execute_complete_tool)
        (
            vec![
                StreamEvent::TextDelta("Brief summary.".into()),
                StreamEvent::MessageStop,
            ],
            10,
            5,
        ),
        // Outer completion resumes with tool result
        (
            vec![
                StreamEvent::TextDelta("Here's what I found: Brief summary.".into()),
                StreamEvent::MessageStop,
            ],
            10,
            5,
        ),
    ]);

    let mut ns = make_namespace(transport);
    seed_user_message(&mut ns, "summarize the project");

    let mut events = vec![];
    run_turn(&mut ns, &mut |e| events.push(format!("{e:?}"))).unwrap();

    // Should have 3 TurnStarts:
    // 1. outer first completion
    // 2. outer second completion (after tool result)
    // Wait — the inner complete call doesn't go through run_turn's loop,
    // so there should be 2 TurnStarts (2 outer loop iterations).
    let turn_starts = events.iter().filter(|e| e.contains("TurnStart")).count();
    assert_eq!(turn_starts, 2);

    // The complete tool call should be visible
    assert!(events.iter().any(|e| e.contains("ToolCallStart")));
    assert!(events.iter().any(|e| e.contains("ToolCallResult")));

    // Final response should include "what I found"
    assert!(events
        .iter()
        .any(|e| e.contains("Here's what I found")));
}
```

- [ ] **Step 2: Update re-exports**

In `crates/ox-core/src/lib.rs`, add to the `pub use ox_kernel::{...}` block:
`ContextRef, ResolvedContext, resolve_refs, complete, default_refs`

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-core -v && cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: All gates pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-core/src/lib.rs
git commit -m "feat(ox-core): integration test for LLM calling complete tool, update re-exports"
```
