pub mod completion;
pub mod fs;
pub mod name_map;
pub mod os;
pub mod policy_store;
pub mod sandbox;
pub mod turn;

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::completion::CompletionModule;
use crate::fs::FsModule;
use crate::name_map::NameMap;
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
/// - Wire names (e.g. `read_file`) — resolved via NameMap to internal paths
pub struct ToolStore {
    fs: FsModule,
    os: OsModule,
    completions: CompletionModule,
    name_map: NameMap,
    last_result: BTreeMap<String, Value>,
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
            last_result: BTreeMap::new(),
        }
    }

    /// Aggregate tool schemas from all modules.
    pub fn all_schemas(&self) -> Vec<ToolSchemaEntry> {
        let mut schemas = self.fs.schemas();
        schemas.extend(self.os.schemas());
        schemas.extend(self.completions.schemas());
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

    /// Access the name map for wire-name / internal-path translation.
    pub fn name_map(&self) -> &NameMap {
        &self.name_map
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
            "fs" | "os" | "completions" | "schemas" => {
                Some(ResolvedPath::Direct(path))
            }
            _ => {
                // Try wire-name resolution
                if let Some(internal) = self.name_map.to_internal(first) {
                    // internal is like "fs/read" — parse it and append remaining components
                    let parsed = Path::parse(internal).ok()?;
                    let mut components = parsed.components;
                    components.extend(path.components[1..].iter().cloned());
                    Some(ResolvedPath::Resolved(
                        Path::from_components(components),
                    ))
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
                self.completions.write(&sub, data)
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

                self.execute_module(first, &op, &json_input)?;

                Ok(path)
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
// - CompletionModule wraps GateStore (HashMap + Box<dyn Store + Send + Sync>)
// - BTreeMap<String, Value> and NameMap are plain data
