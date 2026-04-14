# Tool Result Abbreviation & Retrieval Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Large tool results are abbreviated in history, and the model can retrieve full/partial results through a redirect tool that reads from the LogStore via StructFS namespace routing.

**Architecture:** LogStore gets new read paths (`results/{id}`, `results/{id}/line_count`, `results/{id}/lines/{offset}/{limit}`). HistoryView abbreviates large tool results in projection. ToolStore gains a "redirect tool" concept — tools whose execution returns a namespace-absolute path instead of an exec handle. `get_tool_output` is the first redirect tool. HostStore detects redirect paths and skips `tools/` re-prefixing. Shell gets `max_lines`.

**Tech Stack:** Rust, StructFS (structfs-core-store, structfs-serde-store), serde_json

---

## File Structure

| File | Responsibility |
|------|----------------|
| `crates/ox-kernel/src/log.rs` (modify) | Add `results/` read paths to LogStore, add `tool_result_output` to SharedLog |
| `crates/ox-history/src/lib.rs` (modify) | Add `abbreviate_tool_result` function, call from `project_messages` |
| `crates/ox-tools/src/lib.rs` (modify) | Add redirect tool handling in ToolStore write path, register `get_tool_output` schema |
| `crates/ox-runtime/src/host_store.rs` (modify) | Detect namespace-absolute redirect paths in `handle_write` |
| `crates/ox-tools/src/os.rs` (modify) | Add `max_lines` to shell schema |
| `crates/ox-tools/src/bin/ox-tool-exec.rs` (modify) | Handle `max_lines` truncation in `op_shell` |

---

### Task 1: LogStore result read paths

Add `results/{tool_use_id}` read paths to LogStore. This is the StructFS-native way to access a tool result by ID.

**Files:**
- Modify: `crates/ox-kernel/src/log.rs`

- [ ] **Step 1: Write test for `SharedLog::tool_result_output`**

Add to the existing `mod tests` in `crates/ox-kernel/src/log.rs`:

```rust
#[test]
fn tool_result_output_found() {
    let log = SharedLog::new();
    log.append(LogEntry::ToolResult {
        id: "tc_abc".into(),
        output: serde_json::Value::String("hello world".into()),
        is_error: false,
        scope: None,
    });
    let result = log.tool_result_output("tc_abc");
    assert_eq!(result, Some(serde_json::Value::String("hello world".into())));
}

#[test]
fn tool_result_output_not_found() {
    let log = SharedLog::new();
    log.append(LogEntry::User {
        content: "hi".into(),
        scope: None,
    });
    assert_eq!(log.tool_result_output("tc_missing"), None);
}

#[test]
fn tool_result_output_returns_last_match() {
    let log = SharedLog::new();
    log.append(LogEntry::ToolResult {
        id: "tc_dup".into(),
        output: serde_json::Value::String("first".into()),
        is_error: false,
        scope: None,
    });
    log.append(LogEntry::ToolResult {
        id: "tc_dup".into(),
        output: serde_json::Value::String("second".into()),
        is_error: false,
        scope: None,
    });
    // Reverse scan — returns the most recent
    assert_eq!(
        log.tool_result_output("tc_dup"),
        Some(serde_json::Value::String("second".into()))
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-kernel -- tool_result_output 2>&1 | tail -10`
Expected: FAIL — `tool_result_output` method doesn't exist

- [ ] **Step 3: Implement `SharedLog::tool_result_output`**

Add to `impl SharedLog` in `crates/ox-kernel/src/log.rs`, after `last_n`:

```rust
/// Find a tool result by its tool_use_id and return the output.
pub fn tool_result_output(&self, tool_use_id: &str) -> Option<serde_json::Value> {
    let entries = self.0.lock().unwrap();
    for entry in entries.iter().rev() {
        if let LogEntry::ToolResult { id, output, .. } = entry {
            if id == tool_use_id {
                return Some(output.clone());
            }
        }
    }
    None
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ox-kernel -- tool_result_output 2>&1 | tail -10`
Expected: 3 tests PASS

- [ ] **Step 5: Write test for LogStore `results/{id}` read**

```rust
#[test]
fn read_result_by_id() {
    let mut log = LogStore::new();
    log.write(
        &path!("append"),
        Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
            "type": "tool_result",
            "id": "tc_42",
            "output": "the full output\nline two\nline three",
            "is_error": false
        }))),
    )
    .unwrap();
    let record = log
        .read(&Path::parse("results/tc_42").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        record.as_value().unwrap(),
        &Value::String("the full output\nline two\nline three".into())
    );
}

#[test]
fn read_result_not_found() {
    let mut log = LogStore::new();
    let result = log.read(&Path::parse("results/tc_missing").unwrap());
    assert!(result.is_err());
}
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test -p ox-kernel -- read_result 2>&1 | tail -10`
Expected: FAIL — `results` path not handled

- [ ] **Step 7: Implement LogStore `results/` read path**

In `LogStore::read`, add a new match arm before the `_ => Ok(None)` arm:

```rust
"results" => {
    if from.len() < 2 {
        return Err(StoreError::store(
            "LogStore",
            "read",
            "results requires tool_use_id: results/{id}",
        ));
    }
    let tool_use_id = &from.components[1];
    let output = self
        .shared
        .tool_result_output(tool_use_id)
        .ok_or_else(|| {
            StoreError::store(
                "LogStore",
                "read",
                format!("no tool result with id: {tool_use_id}"),
            )
        })?;
    let full = match &output {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };

    if from.len() == 2 {
        // results/{id} — full output
        return Ok(Some(Record::parsed(Value::String(full))));
    }

    let sub = from.components[2].as_str();
    match sub {
        "line_count" => {
            let count = full.lines().count() as i64;
            Ok(Some(Record::parsed(Value::Integer(count))))
        }
        "lines" => {
            if from.len() < 5 {
                return Err(StoreError::store(
                    "LogStore",
                    "read",
                    "lines requires offset and limit: results/{id}/lines/{offset}/{limit}",
                ));
            }
            let offset: usize = from.components[3]
                .parse()
                .map_err(|e: std::num::ParseIntError| {
                    StoreError::store("LogStore", "read", e.to_string())
                })?;
            let limit: usize = from.components[4]
                .parse()
                .map_err(|e: std::num::ParseIntError| {
                    StoreError::store("LogStore", "read", e.to_string())
                })?;
            let lines: Vec<&str> = full.lines().collect();
            let total = lines.len();
            let start = offset.min(total);
            let end = (start + limit).min(total);
            let sliced = format!(
                "[lines {}-{} of {}]\n{}",
                start + 1,
                end,
                total,
                lines[start..end].join("\n"),
            );
            Ok(Some(Record::parsed(Value::String(sliced))))
        }
        _ => Err(StoreError::store(
            "LogStore",
            "read",
            format!("unknown results sub-path: {sub}"),
        )),
    }
}
```

- [ ] **Step 8: Write tests for `line_count` and `lines/` sub-paths**

```rust
#[test]
fn read_result_line_count() {
    let mut log = LogStore::new();
    log.write(
        &path!("append"),
        Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
            "type": "tool_result",
            "id": "tc_lc",
            "output": "line1\nline2\nline3",
            "is_error": false
        }))),
    )
    .unwrap();
    let record = log
        .read(&Path::parse("results/tc_lc/line_count").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(record.as_value().unwrap(), &Value::Integer(3));
}

#[test]
fn read_result_lines_slice() {
    let mut log = LogStore::new();
    let output = (0..10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    log.write(
        &path!("append"),
        Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
            "type": "tool_result",
            "id": "tc_sl",
            "output": output,
            "is_error": false
        }))),
    )
    .unwrap();
    let record = log
        .read(&Path::parse("results/tc_sl/lines/2/3").unwrap())
        .unwrap()
        .unwrap();
    let val = match record.as_value().unwrap() {
        Value::String(s) => s.clone(),
        _ => panic!("expected string"),
    };
    assert!(val.starts_with("[lines 3-5 of 10]"));
    assert!(val.contains("line 2"));
    assert!(val.contains("line 3"));
    assert!(val.contains("line 4"));
    assert!(!val.contains("line 5"));
}
```

- [ ] **Step 9: Run all log tests**

Run: `cargo test -p ox-kernel -- log 2>&1 | tail -15`
Expected: all PASS

- [ ] **Step 10: Commit**

```bash
git add crates/ox-kernel/src/log.rs
git commit -m "feat: LogStore results/ read paths — full, line_count, lines/{offset}/{limit}"
```

---

### Task 2: History abbreviation

Abbreviate large tool results in `project_messages()`.

**Files:**
- Modify: `crates/ox-history/src/lib.rs`

- [ ] **Step 1: Write test for abbreviation of large tool results**

Add to `mod tests` in `crates/ox-history/src/lib.rs`:

```rust
#[test]
fn history_abbreviates_large_tool_result() {
    let shared = SharedLog::new();
    // Create a tool result with 100 lines
    let big_output = (0..100)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    shared.append(LogEntry::ToolResult {
        id: "tc_big".into(),
        output: serde_json::Value::String(big_output),
        is_error: false,
        scope: None,
    });
    let mut hv = HistoryView::new(shared);
    let messages = hv.read(&path!("messages")).unwrap().unwrap();
    let json = value_to_json(unwrap_value(messages));
    let arr = json.as_array().unwrap();
    let content = arr[0]["content"].as_array().unwrap();
    let result_str = content[0]["content"].as_str().unwrap();
    // Should contain head lines
    assert!(result_str.contains("line 0"));
    assert!(result_str.contains("line 19"));
    // Should contain omission marker
    assert!(result_str.contains("lines omitted"));
    assert!(result_str.contains("tc_big"));
    // Should contain tail lines
    assert!(result_str.contains("line 99"));
    assert!(result_str.contains("line 80"));
    // Should NOT contain middle lines
    assert!(!result_str.contains("line 40"));
}

#[test]
fn history_does_not_abbreviate_small_tool_result() {
    let shared = SharedLog::new();
    let small_output = (0..10)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    shared.append(LogEntry::ToolResult {
        id: "tc_small".into(),
        output: serde_json::Value::String(small_output.clone()),
        is_error: false,
        scope: None,
    });
    let mut hv = HistoryView::new(shared);
    let messages = hv.read(&path!("messages")).unwrap().unwrap();
    let json = value_to_json(unwrap_value(messages));
    let arr = json.as_array().unwrap();
    let content = arr[0]["content"].as_array().unwrap();
    let result_str = content[0]["content"].as_str().unwrap();
    // Should be the full output, no omission
    assert_eq!(result_str, &small_output);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-history -- abbreviat 2>&1 | tail -10`
Expected: FAIL — `history_abbreviates_large_tool_result` fails (no abbreviation happening yet)

- [ ] **Step 3: Add abbreviation constants and function**

Add after the `Writer` impl block for `HistoryView` (before `#[cfg(test)]`):

```rust
// ---------------------------------------------------------------------------
// Tool result abbreviation
// ---------------------------------------------------------------------------

/// Maximum lines to show in full before abbreviating a tool result.
const ABBREVIATE_THRESHOLD_LINES: usize = 50;

/// Number of lines to keep from the head and tail when abbreviating.
const ABBREVIATE_HEAD_LINES: usize = 20;
const ABBREVIATE_TAIL_LINES: usize = 20;

/// Abbreviate a tool result for history projection.
///
/// Results under the threshold are returned unchanged. Large results show
/// the first and last N lines with an omission marker referencing the
/// tool_use_id so the model can retrieve the full output.
fn abbreviate_tool_result(content: &str, tool_use_id: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= ABBREVIATE_THRESHOLD_LINES {
        return content.to_string();
    }

    let head: Vec<&str> = lines[..ABBREVIATE_HEAD_LINES].to_vec();
    let tail: Vec<&str> = lines[lines.len() - ABBREVIATE_TAIL_LINES..].to_vec();
    let omitted = lines.len() - ABBREVIATE_HEAD_LINES - ABBREVIATE_TAIL_LINES;

    format!(
        "{}\n\n[... {omitted} lines omitted — use get_tool_output with \
         tool_use_id=\"{tool_use_id}\" to see full output, \
         or re-run the command with max_lines to limit output at the source]\n\n{}",
        head.join("\n"),
        tail.join("\n"),
    )
}
```

- [ ] **Step 4: Wire abbreviation into `project_messages`**

In `project_messages()`, change the tool result content construction. Replace:

```rust
let content_str = match output {
    serde_json::Value::String(s) => s.clone(),
    other => serde_json::to_string(other).unwrap_or_default(),
};
result_blocks.push(serde_json::json!({
    "type": "tool_result",
    "tool_use_id": id,
    "content": content_str,
}));
```

With:

```rust
let content_str = match output {
    serde_json::Value::String(s) => s.clone(),
    other => serde_json::to_string(other).unwrap_or_default(),
};
let abbreviated = abbreviate_tool_result(&content_str, id);
result_blocks.push(serde_json::json!({
    "type": "tool_result",
    "tool_use_id": id,
    "content": abbreviated,
}));
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-history 2>&1 | tail -15`
Expected: all PASS (including existing tests — small results unchanged)

- [ ] **Step 6: Commit**

```bash
git add crates/ox-history/src/lib.rs
git commit -m "feat: abbreviate large tool results in history projection"
```

---

### Task 3: Redirect tool concept in ToolStore

Add a `redirect_tools` map to ToolStore — tools whose execution returns a namespace path.

**Files:**
- Modify: `crates/ox-tools/src/lib.rs`

- [ ] **Step 1: Write test for redirect tool dispatch**

Add a new test in `crates/ox-tools/src/lib.rs` (or the existing test file). First, check if there's an existing test module:

The ToolStore tests live in `crates/ox-tools/tests/toolstore_integration.rs`. Add a new test file or add to the existing one. For simplicity, add a unit test inside `lib.rs`:

Add at the bottom of `crates/ox-tools/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn redirect_tool_returns_namespace_path() {
        let mut store = ToolStore::empty();
        store.register_redirect(RedirectTool {
            wire_name: "my_redirect".into(),
            internal_path: "redirect/my_redirect".into(),
            description: "test redirect".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
            build_path: Box::new(|input| {
                let id = input.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
                Ok(format!("some/namespace/{id}"))
            }),
        });

        // Write triggers the redirect — returns a namespace path, not exec/NNNN
        let input = structfs_serde_store::json_to_value(serde_json::json!({"id": "abc"}));
        let result_path = store
            .write(&path!("my_redirect"), Record::parsed(input))
            .unwrap();

        // The path should be the namespace-absolute redirect, not exec/NNNN
        assert_eq!(result_path.to_string(), "some/namespace/abc");
    }

    #[test]
    fn redirect_tool_appears_in_schemas() {
        let mut store = ToolStore::empty();
        store.register_redirect(RedirectTool {
            wire_name: "my_redirect".into(),
            internal_path: "redirect/my_redirect".into(),
            description: "test redirect".into(),
            input_schema: serde_json::json!({}),
            build_path: Box::new(|_| Ok("target/path".into())),
        });
        let schemas = store.all_schemas();
        assert!(schemas.iter().any(|s| s.wire_name == "my_redirect"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-tools -- redirect 2>&1 | tail -10`
Expected: FAIL — `RedirectTool` and `register_redirect` don't exist

- [ ] **Step 3: Define `RedirectTool` struct and add to ToolStore**

Add after the `ToolSchemaEntry` struct definition in `crates/ox-tools/src/lib.rs`:

```rust
/// A tool whose execution returns a namespace-absolute path for the kernel to read.
///
/// Unlike normal tools (which compute results), redirect tools build a StructFS path
/// from their input. The kernel reads the result from that path through namespace routing.
pub struct RedirectTool {
    pub wire_name: String,
    pub internal_path: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// Given the tool input, build the namespace path to redirect to.
    pub build_path: Box<dyn Fn(&serde_json::Value) -> Result<String, String> + Send + Sync>,
}
```

Add a `redirect_tools` field to `ToolStore`:

```rust
pub struct ToolStore {
    fs: FsModule,
    os: OsModule,
    completions: CompletionModule,
    name_map: NameMap,
    native_tools: HashMap<String, Box<dyn NativeTool>>,
    redirect_tools: HashMap<String, RedirectTool>,
    last_result: BTreeMap<String, Value>,
    exec_counter: u64,
    exec_results: BTreeMap<String, Value>,
}
```

Initialize it as `HashMap::new()` in `ToolStore::new` and `ToolStore::empty`.

Add `register_redirect` method:

```rust
/// Register a redirect tool.
pub fn register_redirect(&mut self, tool: RedirectTool) {
    self.name_map
        .register(&tool.wire_name, &tool.internal_path);
    self.redirect_tools.insert(tool.wire_name.clone(), tool);
}
```

Add redirect tool schemas to `all_schemas`:

```rust
pub fn all_schemas(&self) -> Vec<ToolSchemaEntry> {
    let mut schemas = self.fs.schemas();
    schemas.extend(self.os.schemas());
    schemas.extend(self.completions.schemas());
    for tool in self.native_tools.values() {
        schemas.push(tool.schema());
    }
    for tool in self.redirect_tools.values() {
        schemas.push(ToolSchemaEntry {
            wire_name: tool.wire_name.clone(),
            internal_path: tool.internal_path.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        });
    }
    schemas
}
```

- [ ] **Step 4: Add redirect tool handling to ToolStore::write**

In `ToolStore::write`, add a redirect check after the native tool check (before `resolve_path`):

```rust
// Check redirect tools (after native, before resolve_path)
if !to.is_empty() {
    let wire_name = to.components[0].as_str();
    if let Some(redirect) = self.redirect_tools.get(wire_name) {
        let value = data
            .as_value()
            .ok_or_else(|| {
                StoreError::store(
                    "ToolStore",
                    "redirect",
                    format!("{wire_name}: expected parsed record"),
                )
            })?
            .clone();
        let input_json = structfs_serde_store::value_to_json(value);
        let target = (redirect.build_path)(&input_json).map_err(|e| {
            StoreError::store("ToolStore", "redirect", format!("{wire_name}: {e}"))
        })?;
        return Path::parse(&target)
            .map_err(|e| StoreError::store("ToolStore", "redirect", e.to_string()));
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-tools -- redirect 2>&1 | tail -10`
Expected: 2 tests PASS

- [ ] **Step 6: Run full ox-tools tests**

Run: `cargo test -p ox-tools 2>&1 | tail -15`
Expected: all PASS

- [ ] **Step 7: Commit**

```bash
git add crates/ox-tools/src/lib.rs
git commit -m "feat: redirect tool concept — tools that return namespace paths"
```

---

### Task 4: HostStore redirect detection

HostStore must detect when a tool write returns a namespace-absolute path (not `exec/NNNN`) and skip the `tools/` re-prefix.

**Files:**
- Modify: `crates/ox-runtime/src/host_store.rs`

- [ ] **Step 1: Write test for redirect path passthrough**

Add to tests in `crates/ox-runtime/src/engine.rs` (where HostStore integration is tested) or add a unit test module to `host_store.rs`. Since `host_store.rs` doesn't have tests, look for the right place. The simplest approach: modify the `handle_write` logic and verify in an integration test. For now, the change is small enough to test by verifying compilation + existing tests.

- [ ] **Step 2: Modify `handle_write` to detect redirects**

In `crates/ox-runtime/src/host_store.rs`, in the `handle_write` method, change the `"tools"` arm:

```rust
"tools" => {
    let sub = Path::from_components(path.components[1..].to_vec());
    let result_path = self.effects.tool_store().write(&sub, data)?;
    // Redirect detection: if the returned path starts with "exec",
    // it's a ToolStore-internal handle — prefix with "tools/" so the
    // kernel reads it back through the ToolStore. Otherwise it's a
    // namespace-absolute redirect path — return as-is for the kernel
    // to read through the full namespace.
    if result_path
        .components
        .first()
        .is_some_and(|c| c == "exec")
    {
        let mut components = vec!["tools".to_string()];
        components.extend(result_path.components);
        Ok(Path::from_components(components))
    } else {
        Ok(result_path)
    }
}
```

- [ ] **Step 3: Verify compilation and tests**

Run: `cargo test -p ox-runtime 2>&1 | tail -15`
Expected: all PASS

- [ ] **Step 4: Commit**

```bash
git add crates/ox-runtime/src/host_store.rs
git commit -m "feat: HostStore detects redirect tool paths — skips tools/ prefix"
```

---

### Task 5: Register `get_tool_output` redirect tool

Wire the `get_tool_output` tool into the ToolStore using the redirect mechanism from Task 3.

**Files:**
- Modify: `crates/ox-cli/src/agents.rs` (where ToolStore is constructed)

- [ ] **Step 1: Register the redirect tool after ToolStore construction**

In `crates/ox-cli/src/agents.rs`, find where `tool_store` is created (around line 245). After the `ToolStore::new(...)` call, add:

```rust
let tool_store = ox_tools::ToolStore::new(fs_module, os_module, completion_module);

// Register get_tool_output — redirect tool for retrieving abbreviated results
tool_store.register_redirect(ox_tools::RedirectTool {
    wire_name: "get_tool_output".into(),
    internal_path: "redirect/get_tool_output".into(),
    description: "Retrieve the full or partial output of a previous tool call. \
                  Use this when a tool result was abbreviated in the conversation."
        .into(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "tool_use_id": {
                "type": "string",
                "description": "The tool_use_id from the abbreviated result"
            },
            "offset": {
                "type": "integer",
                "description": "0-based line offset to start from (default: 0)"
            },
            "limit": {
                "type": "integer",
                "description": "Maximum number of lines to return (default: all)"
            }
        },
        "required": ["tool_use_id"]
    }),
    build_path: Box::new(|input| {
        let id = input
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .ok_or("missing tool_use_id")?;
        let offset = input.get("offset").and_then(|v| v.as_u64());
        let limit = input.get("limit").and_then(|v| v.as_u64());
        match (offset, limit) {
            (Some(o), Some(l)) => Ok(format!("log/results/{id}/lines/{o}/{l}")),
            (Some(o), None) => Ok(format!("log/results/{id}/lines/{o}/999999")),
            (None, Some(l)) => Ok(format!("log/results/{id}/lines/0/{l}")),
            (None, None) => Ok(format!("log/results/{id}")),
        }
    }),
});
```

Note: `tool_store` needs to be `mut`. Check if it already is; if not, add `mut`.

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p ox-cli 2>&1 | tail -10`
Expected: no errors

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-cli 2>&1 | tail -10`
Expected: all PASS

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/agents.rs
git commit -m "feat: register get_tool_output redirect tool"
```

---

### Task 6: Shell `max_lines` parameter

Add `max_lines` to the shell tool schema and handle truncation in the executor.

**Files:**
- Modify: `crates/ox-tools/src/os.rs` (schema)
- Modify: `crates/ox-tools/src/bin/ox-tool-exec.rs` (truncation)

- [ ] **Step 1: Update shell schema in `os.rs`**

In `crates/ox-tools/src/os.rs`, modify the `schemas()` method. Add `max_lines` to the properties:

```rust
pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
    vec![ToolSchemaEntry {
        wire_name: "shell".to_string(),
        internal_path: "os/shell".to_string(),
        description: "Run a shell command in the workspace directory".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "max_lines": {
                    "type": "integer",
                    "description": "Maximum lines of stdout to return. Use for commands that may produce large output. Omit for full output."
                }
            },
            "required": ["command"]
        }),
    }]
}
```

- [ ] **Step 2: Handle `max_lines` in executor's `op_shell`**

In `crates/ox-tools/src/bin/ox-tool-exec.rs`, modify `op_shell`. After collecting stdout, truncate if `max_lines` is set:

```rust
fn op_shell(args: &serde_json::Value) -> ExecResult {
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'command'".into()),
            };
        }
    };

    let workspace = args.get("workspace").and_then(|v| v.as_str());
    let max_lines = args.get("max_lines").and_then(|v| v.as_u64()).map(|v| v as usize);

    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command);

    if let Some(ws) = workspace {
        cmd.current_dir(ws);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String(format!("spawn error: {e}")),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code().unwrap_or(-1);

    let stdout = if let Some(max) = max_lines {
        let lines: Vec<&str> = stdout.lines().collect();
        if lines.len() > max {
            let truncated = lines[..max].join("\n");
            format!(
                "{truncated}\n[... truncated at {max} lines, {} total]",
                lines.len()
            )
        } else {
            stdout
        }
    } else {
        stdout
    };

    ExecResult {
        ok: true,
        value: serde_json::json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": exit_code,
        }),
    }
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p ox-tools 2>&1 | tail -10`
Expected: no errors

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools 2>&1 | tail -10`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/os.rs crates/ox-tools/src/bin/ox-tool-exec.rs
git commit -m "feat: shell max_lines parameter — source-side output control"
```

---

### Task 7: Format and quality gates

- [ ] **Step 1: Format**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: all gates PASS

- [ ] **Step 3: Commit any formatting changes**

```bash
git add -A
git commit -m "chore: fmt"
```
