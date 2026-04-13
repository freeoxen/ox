# Remaining Typed Protocols

**Date:** 2026-04-12
**Status:** Draft — next session
**Depends on:** Editor widget ownership (2026-04-12)

## Problem

The UI protocol is fully typed (UiCommand, UiSnapshot, ApprovalResponse, InputKeyEvent, EditorSnapshot). But five other store protocols still use raw `Value::Map`/`Value::String` construction and manual destructuring.

## Remaining untyped protocols

### 1. Config store writes (`settings_shell.rs`, `broker_setup.rs`)

`Record::parsed(Value::Null)` for save/delete signals. `write_typed(&path, &string_val)` for value writes. Initial config values as `Value::String`/`Value::Integer`.

**Fix:** Define `ConfigCommand` enum or typed config value writes. `Value::Null` save/delete signals need a typed equivalent (e.g., `ConfigCommand::Save`, `ConfigCommand::Delete { key }`).

### 2. Inbox store writes (`agents.rs`, `app.rs`, `event_loop.rs`)

Thread creation: `Record::parsed(Value::Map({ "title": ... }))`.  
Thread state updates: `Value::Map({ "id": ..., "thread_state": ..., "updated_at": ... })`.  
Archive: `Value::Map({ "inbox_state": "done" })`.

**Fix:** Define typed structs: `CreateThread { title }`, `UpdateThread { id, thread_state, updated_at }`, `ArchiveThread { inbox_state }`.

### 3. History store writes (`agents.rs`)

Message append: `json_to_value(user_json)` → `Record::parsed(...)`.  
Turn clear: `Record::parsed(Value::Null)`.  
Turn streaming: `Value::String(text)`.

**Fix:** These are kernel-level wire format writes. The history store's protocol is defined by ox-kernel's message types. Typing these means defining typed history commands or using the existing kernel types via `write_typed`.

### 4. Approval request writes (`policy_check.rs`)

Builds `Value::Map({ "tool_name": ..., "input_preview": ... })` for approval requests.

**Fix:** Use `write_typed(&path, &ApprovalRequest { tool_name, input_preview })` — the type already exists.

### 5. Config/key_hints reads (`view_state.rs`)

Manual `Value::String` destructuring for model/provider config reads. Key hints read as `Value::Array` of `Value::Map`.

**Fix:** `read_typed::<String>` for config values. Define `KeyHint { key, description }` struct for binding reads.

### 6. Tool schema writes (`agents.rs`)

`structfs_serde_store::to_value(&tool_store.tool_schemas_for_model())` then `Record::parsed(val)` — this is already using serde, just not `write_typed`.

**Fix:** Use `adapter.write_typed(&path, &schemas)`.

## Priority

1. **Approval request** (#4) — easiest, type already exists, one call site
2. **Config reads** (#5) — `read_typed::<String>` is trivial
3. **Tool schema writes** (#6) — already serde, just use write_typed
4. **Inbox store** (#2) — define typed structs
5. **Config store commands** (#1) — define ConfigCommand for save/delete
6. **History store** (#3) — depends on kernel types, largest scope
