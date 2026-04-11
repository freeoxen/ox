pub mod completion;
pub mod fs;
pub mod name_map;
pub mod native;
pub mod os;
pub mod policy_store;
pub mod sandbox;
use std::collections::{BTreeMap, HashMap};

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::completion::CompletionModule;
use crate::fs::FsModule;
use crate::name_map::NameMap;
use crate::native::NativeTool;
use crate::os::OsModule;

/// Describes a single tool's schema for registration with the agent framework.
#[derive(Debug, Clone)]
pub struct ToolSchemaEntry {
    /// The wire name exposed to the LLM (e.g. "fs_read").
    pub wire_name: String,
    /// The internal StructFS path used for dispatch (e.g. "fs/read").
    pub internal_path: String,
    /// Human-readable description of the tool.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
}

/// Central StructFS store that routes reads/writes to the appropriate tool module.
///
/// Path routing:
/// - `fs/{op}` — write dispatches to FsModule, read returns last result
/// - `os/{op}` — write dispatches to OsModule, read returns last result
/// - `completions/{...}` — delegated to CompletionModule (GateStore)
/// - `schemas` — read returns aggregated tool schemas as a JSON array
/// - `exec/{id}` — read returns a previously stored execution result (handle pattern)
/// - Wire names (e.g. `read_file`) — resolved via NameMap to internal paths
pub struct ToolStore {
    fs: FsModule,
    os: OsModule,
    completions: CompletionModule,
    name_map: NameMap,
    native_tools: HashMap<String, Box<dyn NativeTool>>,
    last_result: BTreeMap<String, Value>,
    exec_counter: u64,
    exec_results: BTreeMap<String, Value>,
}

impl ToolStore {
    /// Create a new ToolStore, registering name mappings from all modules.
    pub fn new(fs: FsModule, os: OsModule, completions: CompletionModule) -> Self {
        let mut name_map = NameMap::new();

        for schema in fs.schemas() {
            name_map.register(&schema.wire_name, &schema.internal_path);
        }
        for schema in os.schemas() {
            name_map.register(&schema.wire_name, &schema.internal_path);
        }
        for schema in completions.schemas() {
            name_map.register(&schema.wire_name, &schema.internal_path);
        }

        Self {
            fs,
            os,
            completions,
            name_map,
            native_tools: HashMap::new(),
            last_result: BTreeMap::new(),
            exec_counter: 0,
            exec_results: BTreeMap::new(),
        }
    }

    /// Register a native (in-process) tool.
    /// Adds to name_map and native_tools.
    pub fn register_native(&mut self, tool: Box<dyn NativeTool>) {
        let schema = tool.schema();
        self.name_map
            .register(&schema.wire_name, &schema.internal_path);
        self.native_tools.insert(schema.wire_name, tool);
    }

    /// Unregister a native tool by wire name.
    /// Removes from both native_tools and name_map.
    pub fn unregister_native(&mut self, wire_name: &str) {
        self.native_tools.remove(wire_name);
        self.name_map.unregister(wire_name);
    }

    /// Aggregate tool schemas from all modules.
    pub fn all_schemas(&self) -> Vec<ToolSchemaEntry> {
        let mut schemas = self.fs.schemas();
        schemas.extend(self.os.schemas());
        schemas.extend(self.completions.schemas());
        for tool in self.native_tools.values() {
            schemas.push(tool.schema());
        }
        schemas
    }

    /// Convert aggregated schemas to the kernel ToolSchema format.
    pub fn tool_schemas_for_model(&self) -> Vec<ox_kernel::ToolSchema> {
        self.all_schemas()
            .into_iter()
            .map(|entry| ox_kernel::ToolSchema {
                name: entry.wire_name,
                description: entry.description,
                input_schema: entry.input_schema,
            })
            .collect()
    }

    /// Create a ToolStore with stub modules and no registered tools.
    ///
    /// Used as a schema placeholder where tool execution isn't needed
    /// (e.g. thread namespaces that receive schemas via broker writes,
    /// or tests that only need a mountable `Store` at "tools").
    pub fn empty() -> Self {
        use std::sync::Arc;

        let policy: Arc<dyn crate::sandbox::SandboxPolicy> =
            Arc::new(crate::sandbox::PermissivePolicy);
        let workspace = std::path::PathBuf::from(".");
        let executor = std::path::PathBuf::from("ox-tool-exec");

        let fs = crate::fs::FsModule::new(workspace.clone(), executor.clone(), policy.clone());
        let os = crate::os::OsModule::new(workspace, executor, policy);
        let gate = ox_gate::GateStore::new();
        let completions = crate::completion::CompletionModule::new(gate);

        Self::new(fs, os, completions)
    }

    /// Access the name map for wire-name / internal-path translation.
    pub fn name_map(&self) -> &NameMap {
        &self.name_map
    }

    /// Mutable access to the completions module (e.g. to inject a transport).
    pub fn completions_mut(&mut self) -> &mut CompletionModule {
        &mut self.completions
    }

    fn next_exec_id(&mut self) -> String {
        self.exec_counter += 1;
        format!("exec/{:04}", self.exec_counter)
    }

    fn store_exec_result(&mut self, value: Value) -> Path {
        let id = self.next_exec_id();
        self.exec_results.insert(id.clone(), value);
        Path::parse(&id).unwrap()
    }

    /// Resolve the first path component, potentially via wire-name lookup.
    ///
    /// Returns `(module_prefix, op, tail)` where module_prefix is "fs", "os",
    /// or "completions", op is the operation name, and tail is the remaining
    /// path components.
    fn resolve_path<'a>(&self, path: &'a Path) -> Option<ResolvedPath<'a>> {
        if path.is_empty() {
            return None;
        }

        let first = path.components[0].as_str();

        match first {
            "fs" | "os" | "completions" | "schemas" | "exec" => Some(ResolvedPath::Direct(path)),
            _ => {
                // Try wire-name resolution
                if let Some(internal) = self.name_map.to_internal(first) {
                    // internal is like "fs/read" — parse it and append remaining components
                    let parsed = Path::parse(internal).ok()?;
                    let mut components = parsed.components;
                    components.extend(path.components[1..].iter().cloned());
                    Some(ResolvedPath::Resolved(Path::from_components(components)))
                } else {
                    None
                }
            }
        }
    }

    /// Execute a tool operation via its module, storing the result.
    fn execute_module(
        &mut self,
        module: &str,
        op: &str,
        input: &serde_json::Value,
    ) -> Result<Value, StoreError> {
        let internal_path = format!("{module}/{op}");
        let result = match module {
            "fs" => self.fs.execute(op, input),
            "os" => self.os.execute(op, input),
            _ => Err(format!("unknown module: {module}")),
        };

        match result {
            Ok(json_val) => {
                let value = structfs_serde_store::json_to_value(json_val);
                self.last_result.insert(internal_path, value.clone());
                Ok(value)
            }
            Err(e) => {
                let err_value = Value::String(e.clone());
                self.last_result.insert(internal_path, err_value);
                Err(StoreError::store("ToolStore", "execute", e))
            }
        }
    }
}

enum ResolvedPath<'a> {
    /// Path already starts with a known module prefix.
    Direct(&'a Path),
    /// Path was resolved via wire-name lookup.
    Resolved(Path),
}

impl<'a> ResolvedPath<'a> {
    fn as_path(&self) -> &Path {
        match self {
            ResolvedPath::Direct(p) => p,
            ResolvedPath::Resolved(p) => p,
        }
    }
}

impl Reader for ToolStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        // Exec handle reads — returned by writes, keyed like "exec/0001"
        if !from.is_empty() && from.components[0] == "exec" {
            let key = from.components.join("/");
            return Ok(self
                .exec_results
                .get(&key)
                .map(|v| Record::parsed(v.clone())));
        }

        // Check native tools first (keyed by wire name)
        if !from.is_empty() {
            let first = from.components[0].as_str();
            if self.native_tools.contains_key(first) {
                let internal = self
                    .name_map
                    .to_internal(first)
                    .unwrap_or(first)
                    .to_string();
                return Ok(self
                    .last_result
                    .get(&internal)
                    .map(|v| Record::parsed(v.clone())));
            }
        }

        let resolved = match self.resolve_path(from) {
            Some(r) => r,
            None => return Ok(None),
        };

        let path = resolved.as_path();
        let first = path.components[0].as_str();

        match first {
            "schemas" => {
                let schemas = self.all_schemas();
                let json_array: Vec<serde_json::Value> = schemas
                    .into_iter()
                    .map(|s| {
                        serde_json::json!({
                            "name": s.wire_name,
                            "description": s.description,
                            "input_schema": s.input_schema,
                        })
                    })
                    .collect();
                let json = serde_json::Value::Array(json_array);
                let value = structfs_serde_store::json_to_value(json);
                Ok(Some(Record::parsed(value)))
            }
            "completions" => {
                let sub = Path::from_components(path.components[1..].to_vec());
                self.completions.read(&sub)
            }
            "fs" | "os" => {
                // Read pattern: module/{op}/result
                if path.len() < 2 {
                    return Ok(None);
                }
                let op = path.components[1].as_str();
                let internal_path = format!("{first}/{op}");

                // Check if reading a result
                let is_result = path.len() >= 3 && path.components[2] == "result";
                if is_result {
                    Ok(self
                        .last_result
                        .get(&internal_path)
                        .map(|v| Record::parsed(v.clone())))
                } else {
                    // Direct read of last_result for the op
                    Ok(self
                        .last_result
                        .get(&internal_path)
                        .map(|v| Record::parsed(v.clone())))
                }
            }
            _ => Ok(None),
        }
    }
}

impl Writer for ToolStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        // Check native tools first (before resolve_path), keyed by wire name
        if !to.is_empty() {
            let wire_name = to.components[0].as_str().to_string();
            if self.native_tools.contains_key(&wire_name) {
                let value = data
                    .as_value()
                    .ok_or_else(|| {
                        StoreError::store(
                            "ToolStore",
                            "native_write",
                            format!("{wire_name}: expected parsed record"),
                        )
                    })?
                    .clone();
                let input_json = structfs_serde_store::value_to_json(value);
                let internal = self
                    .name_map
                    .to_internal(&wire_name)
                    .unwrap_or(&wire_name)
                    .to_string();
                let result = self
                    .native_tools
                    .get(&wire_name)
                    .unwrap()
                    .execute(input_json)
                    .map_err(|e| {
                        StoreError::store(
                            "ToolStore",
                            "native_execute",
                            format!("{wire_name}: {e}"),
                        )
                    })?;
                let val = structfs_serde_store::json_to_value(result);
                self.last_result.insert(internal, val.clone());
                return Ok(self.store_exec_result(val));
            }
        }

        let resolved = match self.resolve_path(to) {
            Some(r) => r,
            None => {
                return Err(StoreError::store(
                    "ToolStore",
                    "write",
                    format!("unresolvable path: {}", to.components.join("/")),
                ));
            }
        };

        let path = resolved.as_path().clone();
        let first = path.components[0].as_str();

        match first {
            "completions" => {
                let sub = Path::from_components(path.components[1..].to_vec());
                self.completions.write(&sub, data)?;

                // For complete paths, read the response and store as exec handle
                if !sub.is_empty() && sub.components[0] == "complete" {
                    let mut response_components = sub.components.clone();
                    response_components.push("response".to_string());
                    let response_path = Path::from_components(response_components);
                    if let Some(response) = self.completions.read(&response_path)? {
                        let value = response.as_value().cloned().unwrap_or(Value::Null);
                        return Ok(self.store_exec_result(value));
                    }
                }
                Ok(path)
            }
            "fs" | "os" => {
                if path.len() < 2 {
                    return Err(StoreError::store(
                        "ToolStore",
                        "write",
                        format!("missing operation in path: {first}/"),
                    ));
                }
                let op = path.components[1].clone();

                // Extract the serde_json::Value from the record
                let value = data
                    .as_value()
                    .ok_or_else(|| {
                        StoreError::store(
                            "ToolStore",
                            "write",
                            "expected Parsed record for tool execution",
                        )
                    })?
                    .clone();
                let json_input = structfs_serde_store::value_to_json(value);

                let result_value = self.execute_module(first, &op, &json_input)?;

                Ok(self.store_exec_result(result_value))
            }
            _ => Err(StoreError::store(
                "ToolStore",
                "write",
                format!("cannot write to path: {}", path.components.join("/")),
            )),
        }
    }
}

// Send+Sync: All fields are naturally Send+Sync:
// - FsModule/OsModule contain PathBuf + Arc<dyn SandboxPolicy>
// - CompletionModule wraps GateStore (HashMap)
// - BTreeMap<String, Value> and NameMap are plain data
// - HashMap<String, Box<dyn NativeTool>> — NativeTool: Send + Sync
