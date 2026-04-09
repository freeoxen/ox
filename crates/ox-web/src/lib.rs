//! Browser Wasm shell for the ox agent framework.
//!
//! `ox-web` compiles to a `cdylib` Wasm module via `wasm-pack` and exposes
//! [`OxAgent`] to JavaScript. Tools can be registered from JS at runtime,
//! and the agentic loop runs entirely in the browser (fetching completions
//! from the Anthropic API or a local proxy).
//!
//! ```js
//! import init, { create_agent } from "./ox_web.js";
//! await init();
//! const agent = create_agent("You are helpful.", apiKey);
//! const reply = await agent.prompt("Hello");
//! ```

use ox_context::{Namespace, SystemProvider, ToolsProvider};
use ox_core::{AgentEvent, CompletionRequest, ContentBlock, ToolSchema, serialize_tool_results};
use ox_gate::codec::{anthropic as anthropic_codec, openai as openai_codec};
use ox_gate::{AccountConfig, GateStore, ProviderConfig};
use ox_history::HistoryProvider;
use ox_kernel::ModelInfo;
use ox_kernel::{Path, Reader, Record, ToolRegistry, ToolResult, Value, Writer, path};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use structfs_serde_store::{json_to_value, to_value, value_to_json};
use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// JS tool wrapper — allows tools defined in browser JS
// ---------------------------------------------------------------------------

struct JsTool {
    name: String,
    description: String,
    parameters_schema: serde_json::Value,
    callback: js_sys::Function,
}

impl JsTool {
    fn execute(&self, input: &serde_json::Value) -> Result<String, String> {
        let input_str = serde_json::to_string(input).map_err(|e| e.to_string())?;
        let result = self
            .callback
            .call1(&JsValue::NULL, &JsValue::from_str(&input_str))
            .map_err(|e| {
                let msg = js_sys::Reflect::get(&e, &"message".into())
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_else(|| format!("{e:?}"));
                format!("tool threw: {msg}")
            })?;
        result
            .as_string()
            .ok_or_else(|| "tool callback must return a string".to_string())
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.parameters_schema.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wasm-bindgen exports
// ---------------------------------------------------------------------------

/// The main ox agent handle exposed to JavaScript via `wasm-bindgen`.
///
/// Create one with [`create_agent`] or `new OxAgent(systemPrompt, apiKey)` from JS.
/// Register tools with [`register_tool`](OxAgent::register_tool), subscribe to
/// events with [`on_event`](OxAgent::on_event), and drive the agentic loop with
/// [`prompt`](OxAgent::prompt).
#[wasm_bindgen]
pub struct OxAgent {
    context: Rc<RefCell<Namespace>>,
    event_callback: Option<js_sys::Function>,
    rust_tools: Rc<RefCell<ToolRegistry>>,
    js_tools: Rc<RefCell<HashMap<String, JsTool>>>,
}

#[wasm_bindgen]
impl OxAgent {
    #[wasm_bindgen(constructor)]
    pub fn new(system_prompt: &str, api_key: &str) -> Self {
        let model = "claude-sonnet-4-20250514".to_string();
        let max_tokens = 4096;

        let tool_registry = ToolRegistry::new();
        let schemas = tool_registry.schemas();

        let mut gate = GateStore::new();
        if !api_key.is_empty() {
            gate.write(
                &path!("accounts/anthropic/key"),
                Record::parsed(Value::String(api_key.to_string())),
            )
            .ok();
        }

        let mut context = Namespace::new();
        context.mount(
            "system",
            Box::new(SystemProvider::new(system_prompt.to_string())),
        );
        context.mount("history", Box::new(HistoryProvider::new()));
        context.mount("tools", Box::new(ToolsProvider::new(schemas)));
        context.mount("gate", Box::new(gate));

        context
            .write(
                &path!("gate/defaults/model"),
                Record::parsed(Value::String(model)),
            )
            .ok();
        context
            .write(
                &path!("gate/defaults/max_tokens"),
                Record::parsed(Value::Integer(max_tokens as i64)),
            )
            .ok();

        Self {
            context: Rc::new(RefCell::new(context)),
            event_callback: None,
            rust_tools: Rc::new(RefCell::new(tool_registry)),
            js_tools: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Set an API key for the given provider (e.g. "anthropic", "openai").
    pub fn set_api_key(&self, provider: &str, key: &str) -> Result<(), JsValue> {
        let mut ctx = self.context.borrow_mut();

        // If no account for this provider exists, create one
        let key_path = Path::from_components(vec![
            "gate".to_string(),
            "accounts".to_string(),
            provider.to_string(),
            "key".to_string(),
        ]);
        let has_account = ctx.read(&key_path).ok().flatten().is_some();
        if !has_account {
            let config = AccountConfig {
                provider: provider.to_string(),
                key: key.to_string(),
            };
            let value = to_value(&config).map_err(|e| JsValue::from_str(&e.to_string()))?;
            let account_path = Path::from_components(vec![
                "gate".to_string(),
                "accounts".to_string(),
                provider.to_string(),
            ]);
            ctx.write(&account_path, Record::parsed(value))
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        } else {
            let key_path = Path::from_components(vec![
                "gate".to_string(),
                "accounts".to_string(),
                provider.to_string(),
                "key".to_string(),
            ]);
            ctx.write(&key_path, Record::parsed(Value::String(key.to_string())))
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        }
        Ok(())
    }

    /// Remove the API key for the given provider.
    pub fn remove_api_key(&self, provider: &str) -> Result<(), JsValue> {
        let key_path = Path::from_components(vec![
            "gate".to_string(),
            "accounts".to_string(),
            provider.to_string(),
            "key".to_string(),
        ]);
        self.context
            .borrow_mut()
            .write(&key_path, Record::parsed(Value::String(String::new())))
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(())
    }

    /// Check whether an API key is set for the given provider.
    pub fn has_api_key(&self, provider: &str) -> bool {
        let key_path = Path::from_components(vec![
            "gate".to_string(),
            "accounts".to_string(),
            provider.to_string(),
            "key".to_string(),
        ]);
        let mut ctx = self.context.borrow_mut();
        match ctx.read(&key_path) {
            Ok(Some(Record::Parsed(Value::String(s)))) => !s.is_empty(),
            _ => false,
        }
    }

    /// Set the active provider (writes bootstrap account name).
    pub fn set_provider(&self, provider: &str) -> Result<(), JsValue> {
        self.context
            .borrow_mut()
            .write(
                &path!("gate/defaults/account"),
                Record::parsed(Value::String(provider.to_string())),
            )
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if let Some(ref cb) = self.event_callback {
            emit_js(Some(cb), "context_changed", "");
        }
        Ok(())
    }

    /// Get the current active provider.
    pub fn get_provider(&self) -> String {
        let mut ctx = self.context.borrow_mut();
        // Read bootstrap account name, then read that account's provider
        let bootstrap = match ctx.read(&path!("gate/defaults/account")) {
            Ok(Some(Record::Parsed(Value::String(s)))) => s,
            _ => return "anthropic".to_string(),
        };
        let provider_path = Path::from_components(vec![
            "gate".to_string(),
            "accounts".to_string(),
            bootstrap,
            "provider".to_string(),
        ]);
        match ctx.read(&provider_path) {
            Ok(Some(Record::Parsed(Value::String(s)))) => s,
            _ => "anthropic".to_string(),
        }
    }

    /// Register a JS callback to receive agent events.
    pub fn on_event(&mut self, callback: js_sys::Function) {
        self.event_callback = Some(callback);
    }

    /// Unregister a JS tool by name.
    pub fn unregister_tool(&self, name: &str) {
        self.js_tools.borrow_mut().remove(name);
        self.rebuild_tools_provider();
        if let Some(ref cb) = self.event_callback {
            emit_js(Some(cb), "context_changed", "");
        }
    }

    /// Register a tool implemented in JS.
    ///
    /// The callback receives a JSON string of the tool input and must return
    /// a string result.
    pub fn register_tool(
        &self,
        name: &str,
        description: &str,
        parameters_schema_json: &str,
        callback: js_sys::Function,
    ) -> Result<(), JsValue> {
        let schema: serde_json::Value = serde_json::from_str(parameters_schema_json)
            .map_err(|e| JsValue::from_str(&format!("invalid parameters_schema JSON: {e}")))?;

        self.js_tools.borrow_mut().insert(
            name.to_string(),
            JsTool {
                name: name.to_string(),
                description: description.to_string(),
                parameters_schema: schema,
                callback,
            },
        );

        self.rebuild_tools_provider();

        if let Some(ref cb) = self.event_callback {
            emit_js(Some(cb), "context_changed", "");
        }
        Ok(())
    }

    /// Read the full namespace state for debugging.
    /// Returns a JSON string with system, model, tools, and history.
    pub fn debug_context(&self) -> String {
        let mut ctx = self.context.borrow_mut();

        let system = ctx
            .read(&path!("system"))
            .ok()
            .flatten()
            .map(record_to_json);
        let model_id = ctx
            .read(&path!("gate/defaults/model"))
            .ok()
            .flatten()
            .map(record_to_json);
        let model_max_tokens = ctx
            .read(&path!("gate/defaults/max_tokens"))
            .ok()
            .flatten()
            .map(record_to_json);
        let tools = ctx
            .read(&path!("tools/schemas"))
            .ok()
            .flatten()
            .map(record_to_json);
        let history_count = ctx
            .read(&path!("history/count"))
            .ok()
            .flatten()
            .map(record_to_json);
        let history_messages = ctx
            .read(&path!("history/messages"))
            .ok()
            .flatten()
            .map(record_to_json);

        let gate_bootstrap = ctx
            .read(&path!("gate/defaults/account"))
            .ok()
            .flatten()
            .map(record_to_json);

        let snapshot = serde_json::json!({
            "system": system,
            "model": {
                "id": model_id,
                "max_tokens": model_max_tokens,
            },
            "tools": tools,
            "history": {
                "count": history_count,
                "messages": history_messages,
            },
            "gate": {
                "bootstrap": gate_bootstrap,
            },
        });
        snapshot.to_string()
    }

    /// Replace the system prompt in the namespace.
    pub fn set_system_prompt(&self, new_prompt: &str) -> Result<(), JsValue> {
        let record = Record::parsed(Value::String(new_prompt.to_string()));
        self.context
            .borrow_mut()
            .write(&path!("system"), record)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if let Some(ref cb) = self.event_callback {
            emit_js(Some(cb), "context_changed", "");
        }
        Ok(())
    }

    /// Change the model used for completions.
    pub fn set_model(&self, model_id: &str) -> Result<(), JsValue> {
        let record = Record::parsed(Value::String(model_id.to_string()));
        self.context
            .borrow_mut()
            .write(&path!("gate/defaults/model"), record)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if let Some(ref cb) = self.event_callback {
            emit_js(Some(cb), "context_changed", "");
        }
        Ok(())
    }

    /// Read the model catalog from the namespace.
    /// Returns a JSON array of `{id, display_name}` objects.
    pub fn list_models(&self) -> String {
        let mut ctx = self.context.borrow_mut();
        let provider = match ctx.read(&path!("gate/defaults/account")) {
            Ok(Some(Record::Parsed(Value::String(s)))) => s,
            _ => "anthropic".to_string(),
        };
        let models_path = Path::from_components(vec![
            "gate".to_string(),
            "providers".to_string(),
            provider,
            "models".to_string(),
        ]);
        let catalog = ctx
            .read(&models_path)
            .ok()
            .flatten()
            .map(record_to_json)
            .unwrap_or(serde_json::Value::Array(vec![]));
        catalog.to_string()
    }

    /// Fetch available models from the current provider and write to the catalog.
    /// Returns a Promise that resolves with the JSON catalog array.
    pub fn refresh_models(&self) -> js_sys::Promise {
        let provider = self.get_provider();
        let api_key = read_api_key(&self.context, &provider);
        let config = read_provider_config(&self.context, &provider);
        let context = self.context.clone();
        let callback = self.event_callback.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let models = fetch_model_catalog(&config, &api_key).await?;
            let value = structfs_serde_store::to_value(&models)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            let models_path = Path::from_components(vec![
                "gate".to_string(),
                "providers".to_string(),
                provider,
                "models".to_string(),
            ]);
            context
                .borrow_mut()
                .write(&models_path, Record::parsed(value))
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            if let Some(ref cb) = callback {
                emit_js(Some(cb), "context_changed", "");
            }
            let json =
                serde_json::to_string(&models).map_err(|e| JsValue::from_str(&e.to_string()))?;
            Ok(JsValue::from_str(&json))
        })
    }

    /// Send a user prompt and run the agentic loop to completion.
    /// Returns a Promise that resolves with the final assistant text.
    pub fn prompt(&self, input: &str) -> js_sys::Promise {
        let input = input.to_string();
        let callback = self.event_callback.clone();
        let context = self.context.clone();
        let rust_tools = self.rust_tools.clone();
        let js_tools = self.js_tools.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let result =
                run_agentic_loop(&input, &context, &rust_tools, &js_tools, callback.as_ref()).await;

            match result {
                Ok(text) => Ok(JsValue::from_str(&text)),
                Err(e) => Err(JsValue::from_str(&e)),
            }
        })
    }
}

impl OxAgent {
    /// Rebuild the ToolsProvider in the namespace from both Rust and JS tools.
    fn rebuild_tools_provider(&self) {
        let mut schemas = self.rust_tools.borrow().schemas();
        for jt in self.js_tools.borrow().values() {
            schemas.push(jt.schema());
        }
        self.context
            .borrow_mut()
            .mount("tools", Box::new(ToolsProvider::new(schemas)));
    }
}

/// Read the API key for the given account from the gate store.
fn read_api_key(context: &Rc<RefCell<Namespace>>, account: &str) -> String {
    let key_path = Path::from_components(vec![
        "gate".to_string(),
        "accounts".to_string(),
        account.to_string(),
        "key".to_string(),
    ]);
    let mut ctx = context.borrow_mut();
    match ctx.read(&key_path) {
        Ok(Some(Record::Parsed(Value::String(s)))) => s,
        _ => String::new(),
    }
}

/// Read the ProviderConfig for the given provider name from the gate store.
fn read_provider_config(context: &Rc<RefCell<Namespace>>, provider: &str) -> ProviderConfig {
    let provider_path = Path::from_components(vec![
        "gate".to_string(),
        "providers".to_string(),
        provider.to_string(),
    ]);
    let mut ctx = context.borrow_mut();
    match ctx.read(&provider_path) {
        Ok(Some(Record::Parsed(v))) => {
            structfs_serde_store::from_value(v).unwrap_or_else(|_| ProviderConfig::anthropic())
        }
        _ => ProviderConfig::anthropic(),
    }
}

/// Convert a Record to serde_json::Value for debug output.
fn record_to_json(record: Record) -> serde_json::Value {
    match record {
        Record::Parsed(v) => value_to_json(v),
        _ => serde_json::Value::Null,
    }
}

fn emit_js(callback: Option<&js_sys::Function>, event_type: &str, data: &str) {
    if let Some(cb) = callback {
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"type".into(), &event_type.into()).ok();
        js_sys::Reflect::set(&obj, &"data".into(), &data.into()).ok();
        cb.call1(&JsValue::NULL, &obj).ok();
    }
}

/// Fetch a completion from an LLM provider, dispatching by dialect.
async fn fetch_completion(
    config: &ProviderConfig,
    api_key: &str,
    request: &CompletionRequest,
) -> Result<String, String> {
    let request_body = match config.dialect.as_str() {
        "openai" => {
            let body = openai_codec::translate_request(request);
            serde_json::to_string(&body).map_err(|e| e.to_string())?
        }
        _ => serde_json::to_string(request).map_err(|e| e.to_string())?,
    };

    let window = web_sys::window().ok_or("no window")?;
    let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| format!("{e:?}"))?;

    match config.dialect.as_str() {
        "openai" => {
            headers
                .set("Authorization", &format!("Bearer {api_key}"))
                .map_err(|e| format!("{e:?}"))?;
        }
        _ => {
            headers
                .set("x-api-key", api_key)
                .map_err(|e| format!("{e:?}"))?;
            if !config.version.is_empty() {
                headers
                    .set("anthropic-version", &config.version)
                    .map_err(|e| format!("{e:?}"))?;
            }
            headers
                .set("anthropic-dangerous-direct-browser-access", "true")
                .map_err(|e| format!("{e:?}"))?;
        }
    }

    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(web_sys::RequestMode::Cors);
    opts.set_body(&JsValue::from_str(&request_body));
    opts.set_headers(&headers);

    let endpoint = &config.endpoint;
    let request =
        web_sys::Request::new_with_str_and_init(endpoint, &opts).map_err(|e| format!("{e:?}"))?;

    let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| {
            let msg = js_sys::Reflect::get(&e, &"message".into())
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            if msg.contains("Failed to fetch") {
                format!("Could not reach {endpoint} — check your network connection")
            } else {
                format!("fetch error: {msg}")
            }
        })?;

    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|_| "response is not a Response".to_string())?;

    if !resp.ok() {
        let status = resp.status();
        let text = wasm_bindgen_futures::JsFuture::from(resp.text().map_err(|e| format!("{e:?}"))?)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let body = text.as_string().unwrap_or_default();
        return match status {
            401 => Err("Invalid API key — check your API key and try again".to_string()),
            _ => Err(format!("HTTP {status}: {body}")),
        };
    }

    let text = wasm_bindgen_futures::JsFuture::from(resp.text().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("{e:?}"))?;

    text.as_string()
        .ok_or_else(|| "response body not a string".into())
}

/// Fetch the model catalog from a provider, dispatching by dialect.
async fn fetch_model_catalog(
    config: &ProviderConfig,
    api_key: &str,
) -> Result<Vec<ModelInfo>, JsValue> {
    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;

    let headers = web_sys::Headers::new().map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
    match config.dialect.as_str() {
        "openai" => {
            headers
                .set("Authorization", &format!("Bearer {api_key}"))
                .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        }
        _ => {
            headers
                .set("x-api-key", api_key)
                .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
            if !config.version.is_empty() {
                headers
                    .set("anthropic-version", &config.version)
                    .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
            }
            headers
                .set("anthropic-dangerous-direct-browser-access", "true")
                .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        }
    }

    // Derive the models list endpoint from the completion endpoint.
    // "https://api.anthropic.com/v1/messages" → "https://api.anthropic.com/v1/models"
    // "https://api.openai.com/v1/chat/completions" → "https://api.openai.com/v1/models"
    let base = config
        .endpoint
        .rfind("/v1/")
        .map(|i| &config.endpoint[..i + 4])
        .unwrap_or(&config.endpoint);
    let models_base = format!("{base}models");

    let mut all_models: Vec<ModelInfo> = Vec::new();
    let mut after_id: Option<String> = None;

    loop {
        let url = match (&after_id, config.dialect.as_str()) {
            (Some(cursor), "anthropic") => {
                format!("{models_base}?limit=1000&after_id={cursor}")
            }
            (None, "anthropic") => format!("{models_base}?limit=1000"),
            _ => models_base.clone(),
        };

        let opts = web_sys::RequestInit::new();
        opts.set_method("GET");
        opts.set_mode(web_sys::RequestMode::Cors);
        opts.set_headers(&headers);

        let request = web_sys::Request::new_with_str_and_init(&url, &opts)
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;

        let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|e| {
                let msg = js_sys::Reflect::get(&e, &"message".into())
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                JsValue::from_str(&format!("fetch error: {msg}"))
            })?;

        let resp: web_sys::Response = resp_value
            .dyn_into()
            .map_err(|_| JsValue::from_str("response is not a Response"))?;

        if !resp.ok() {
            let status = resp.status();
            let text = wasm_bindgen_futures::JsFuture::from(
                resp.text()
                    .map_err(|e| JsValue::from_str(&format!("{e:?}")))?,
            )
            .await
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
            let body = text.as_string().unwrap_or_default();
            return Err(JsValue::from_str(&format!("HTTP {status}: {body}")));
        }

        let text = wasm_bindgen_futures::JsFuture::from(
            resp.text()
                .map_err(|e| JsValue::from_str(&format!("{e:?}")))?,
        )
        .await
        .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        let body_str = text
            .as_string()
            .ok_or_else(|| JsValue::from_str("response body not a string"))?;

        let page: serde_json::Value =
            serde_json::from_str(&body_str).map_err(|e| JsValue::from_str(&e.to_string()))?;

        if let Some(data) = page.get("data").and_then(|d| d.as_array()) {
            for entry in data {
                let id = entry
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let display_name = entry
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&id)
                    .to_string();

                // OpenAI returns everything — filter to chat models
                if config.dialect == "openai"
                    && !(id.contains("gpt")
                        || id.contains("o1")
                        || id.contains("o3")
                        || id.contains("o4"))
                {
                    continue;
                }

                all_models.push(ModelInfo { id, display_name });
            }
        }

        // Only Anthropic paginates with has_more/last_id
        let has_more = page
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !has_more {
            break;
        }
        after_id = page
            .get("last_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    all_models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(all_models)
}

/// Execute a tool call against the Rust registry first, then JS tools.
fn execute_tool(
    rust_tools: &Rc<RefCell<ToolRegistry>>,
    js_tools: &Rc<RefCell<HashMap<String, JsTool>>>,
    name: &str,
    input: &serde_json::Value,
) -> String {
    {
        let registry = rust_tools.borrow();
        if let Some(tool) = registry.get(name) {
            return match tool.execute(input.clone()) {
                Ok(r) => r,
                Err(e) => format!("error: {e}"),
            };
        }
    }
    {
        let js = js_tools.borrow();
        if let Some(jt) = js.get(name) {
            return match jt.execute(input) {
                Ok(r) => r,
                Err(e) => format!("error: {e}"),
            };
        }
    }
    format!("error: unknown tool '{name}'")
}

/// Run the full agentic loop: prompt -> stream -> tool call -> result -> loop.
async fn run_agentic_loop(
    user_input: &str,
    context_ref: &Rc<RefCell<Namespace>>,
    rust_tools: &Rc<RefCell<ToolRegistry>>,
    js_tools: &Rc<RefCell<HashMap<String, JsTool>>>,
    callback: Option<&js_sys::Function>,
) -> Result<String, String> {
    // Write user message to the namespace
    let user_json = serde_json::json!({
        "role": "user",
        "content": user_input,
    });
    let record = Record::parsed(json_to_value(user_json));
    context_ref
        .borrow_mut()
        .write(&path!("history/append"), record)
        .map_err(|e| e.to_string())?;
    emit_js(callback, "context_changed", "");

    // Read bootstrap account → provider config from gate
    let bootstrap = {
        let mut ctx = context_ref.borrow_mut();
        match ctx.read(&path!("gate/defaults/account")) {
            Ok(Some(Record::Parsed(Value::String(s)))) => s,
            _ => "anthropic".to_string(),
        }
    };
    let provider = {
        let provider_path = Path::from_components(vec![
            "gate".to_string(),
            "accounts".to_string(),
            bootstrap.clone(),
            "provider".to_string(),
        ]);
        let mut ctx = context_ref.borrow_mut();
        match ctx.read(&provider_path) {
            Ok(Some(Record::Parsed(Value::String(s)))) => s,
            _ => "anthropic".to_string(),
        }
    };
    let provider_config = read_provider_config(context_ref, &provider);

    let api_key = read_api_key(context_ref, &bootstrap);

    if api_key.is_empty() {
        return Err(format!("No API key set for provider '{provider}'"));
    }

    let model_id = {
        let record = context_ref
            .borrow_mut()
            .read(&path!("gate/defaults/model"))
            .map_err(|e| e.to_string())?;
        match record {
            Some(Record::Parsed(Value::String(s))) => s,
            _ => String::new(),
        }
    };
    let mut kernel = ox_kernel::Kernel::new(model_id);

    let mut emit = |event: AgentEvent| match &event {
        AgentEvent::TextDelta(text) => emit_js(callback, "text_delta", text),
        AgentEvent::ToolCallStart { name } => emit_js(callback, "tool_call_start", name),
        AgentEvent::ToolCallResult { name, result } => {
            emit_js(callback, "tool_call_result", &format!("{name}: {result}"))
        }
        AgentEvent::TurnEnd => emit_js(callback, "turn_end", ""),
        AgentEvent::Error(e) => emit_js(callback, "error", e),
        _ => {}
    };

    loop {
        // Phase 1: read prompt from namespace
        let request = kernel
            .initiate_completion(&mut *context_ref.borrow_mut())
            .map_err(|e| e.to_string())?;

        let request_json = serde_json::to_string(&request).map_err(|e| e.to_string())?;
        emit_js(callback, "request_sent", &request_json);

        // Async fetch (the yield point that motivates three-phase design)
        let response_body = fetch_completion(&provider_config, &api_key, &request).await?;
        let (events, usage) = match provider_config.dialect.as_str() {
            "openai" => openai_codec::parse_sse_events(&response_body),
            _ => {
                let usage = anthropic_codec::extract_usage(&response_body);
                let evts = anthropic_codec::parse_sse_events(&response_body);
                (evts, usage)
            }
        };

        // Emit usage event
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            emit_js(
                callback,
                "usage",
                &serde_json::json!({
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                })
                .to_string(),
            );
        }

        // Phase 2: accumulate events into content blocks
        let content = kernel.consume_events(events, &mut emit)?;

        // Phase 3: write assistant message, extract tool calls
        let tool_calls = kernel.complete_turn(&mut *context_ref.borrow_mut(), &content)?;
        emit_js(callback, "context_changed", "");

        if tool_calls.is_empty() {
            let text = content
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");

            emit_js(callback, "turn_end", "");
            return Ok(text);
        }

        // Execute tools (Rust registry + JS tools)
        let mut results = Vec::new();
        for tc in &tool_calls {
            emit_js(callback, "tool_call_start", &tc.name);
            let result_str = execute_tool(rust_tools, js_tools, &tc.name, &tc.input);
            emit_js(
                callback,
                "tool_call_result",
                &format!("{}: {}", tc.name, result_str),
            );
            results.push(ToolResult {
                tool_use_id: tc.id.clone(),
                content: result_str,
            });
        }

        // Write tool results to history
        let results_json = serialize_tool_results(&results);
        let record = Record::parsed(json_to_value(results_json));
        context_ref
            .borrow_mut()
            .write(&path!("history/append"), record)
            .map_err(|e| e.to_string())?;
        emit_js(callback, "context_changed", "");
    }
}

/// Create an agent with direct Anthropic API access. Exported to JS.
#[wasm_bindgen]
pub fn create_agent(system_prompt: &str, api_key: &str) -> OxAgent {
    OxAgent::new(system_prompt, api_key)
}
