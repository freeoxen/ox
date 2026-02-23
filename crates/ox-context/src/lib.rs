use ox_kernel::{CompletionRequest, Provider, Value};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Namespace — routes reads/writes to mounted providers by path prefix
// ---------------------------------------------------------------------------

/// A namespace routes reads and writes to mounted providers by path prefix.
///
/// Paths are split on the first `/`: `"history/messages"` routes to the
/// provider mounted at `"history"` with sub-path `"messages"`.
///
/// The special path `"prompt"` is synthetic — it assembles a CompletionRequest
/// by reading from sibling providers (system, history, tools, model).
pub struct Namespace {
    mounts: BTreeMap<String, Box<dyn Provider>>,
}

impl Namespace {
    pub fn new() -> Self {
        Self {
            mounts: BTreeMap::new(),
        }
    }

    pub fn mount(&mut self, prefix: &str, provider: Box<dyn Provider>) {
        self.mounts.insert(prefix.to_string(), provider);
    }

    fn synthesize_prompt(&self) -> Option<Value> {
        let system = self.mounts.get("system")?.read("")?;
        let messages = self.mounts.get("history")?.read("messages")?;
        let tools = self.mounts.get("tools")?.read("schemas")?;
        let model_id = self.mounts.get("model")?.read("id")?;
        let max_tokens = self.mounts.get("model")?.read("max_tokens")?;

        let request = CompletionRequest {
            model: system.as_str()?.to_string(), // placeholder — replaced below
            max_tokens: 0,                       // placeholder — replaced below
            system: system.as_str()?.to_string(),
            messages: messages.as_array()?.clone(),
            tools: serde_json::from_value(tools).ok()?,
            stream: true,
        };

        // Fix model and max_tokens from their actual values
        let request = CompletionRequest {
            model: model_id.as_str()?.to_string(),
            max_tokens: max_tokens.as_u64()? as u32,
            ..request
        };

        serde_json::to_value(&request).ok()
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for Namespace {
    fn read(&self, path: &str) -> Option<Value> {
        // Special synthetic reads
        if path == "prompt" {
            return self.synthesize_prompt();
        }

        let (prefix, sub) = split_path(path);
        self.mounts.get(prefix)?.read(sub)
    }

    fn write(&mut self, path: &str, value: Value) -> Result<(), String> {
        let (prefix, sub) = split_path(path);
        self.mounts
            .get_mut(prefix)
            .ok_or_else(|| format!("no provider mounted at '{prefix}'"))?
            .write(sub, value)
    }
}

/// Split `"history/messages"` into `("history", "messages")`.
/// A bare `"history"` yields `("history", "")`.
fn split_path(path: &str) -> (&str, &str) {
    let path = path.strip_prefix('/').unwrap_or(path);
    match path.split_once('/') {
        Some((prefix, rest)) => (prefix, rest),
        None => (path, ""),
    }
}

// ---------------------------------------------------------------------------
// Concrete providers: System, Tools, Model
// ---------------------------------------------------------------------------

/// Provides the system prompt string.
pub struct SystemProvider {
    prompt: String,
}

impl SystemProvider {
    pub fn new(prompt: String) -> Self {
        Self { prompt }
    }
}

impl Provider for SystemProvider {
    fn read(&self, _path: &str) -> Option<Value> {
        Some(Value::String(self.prompt.clone()))
    }

    fn write(&mut self, _path: &str, value: Value) -> Result<(), String> {
        self.prompt = value.as_str().ok_or("expected string")?.to_string();
        Ok(())
    }
}

/// Provides tool schemas (read-only snapshot).
pub struct ToolsProvider {
    schemas: Vec<ox_kernel::ToolSchema>,
}

impl ToolsProvider {
    pub fn new(schemas: Vec<ox_kernel::ToolSchema>) -> Self {
        Self { schemas }
    }
}

impl Provider for ToolsProvider {
    fn read(&self, path: &str) -> Option<Value> {
        match path {
            "" | "schemas" => serde_json::to_value(&self.schemas).ok(),
            _ => None,
        }
    }

    fn write(&mut self, _path: &str, _value: Value) -> Result<(), String> {
        Err("tools provider is read-only".to_string())
    }
}

/// Provides model ID and max_tokens.
pub struct ModelProvider {
    model: String,
    max_tokens: u32,
}

impl ModelProvider {
    pub fn new(model: String, max_tokens: u32) -> Self {
        Self { model, max_tokens }
    }
}

impl Provider for ModelProvider {
    fn read(&self, path: &str) -> Option<Value> {
        match path {
            "" | "id" => Some(Value::String(self.model.clone())),
            "max_tokens" => Some(serde_json::json!(self.max_tokens)),
            _ => None,
        }
    }

    fn write(&mut self, path: &str, value: Value) -> Result<(), String> {
        match path {
            "" | "id" => {
                self.model = value.as_str().ok_or("expected string")?.to_string();
                Ok(())
            }
            "max_tokens" => {
                self.max_tokens = value.as_u64().ok_or("expected number")? as u32;
                Ok(())
            }
            _ => Err(format!("unknown path: {path}")),
        }
    }
}
