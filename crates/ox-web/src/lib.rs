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

use ox_context::{ModelInfo, ModelProvider, Namespace, SystemProvider, ToolsProvider};
use ox_core::{
    AgentEvent, CompletionRequest, ContentBlock, EventStream, StreamEvent, ToolSchema, Transport,
    serialize_assistant_message, serialize_tool_results,
};
use ox_history::HistoryProvider;
use ox_kernel::{Reader, Record, ToolRegistry, ToolResult, Value, Writer, path};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use structfs_serde_store::{json_to_value, value_to_json};
use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// PreloadedTransport — wraps a pre-fetched BufferedStream
// ---------------------------------------------------------------------------

/// A buffered stream of pre-parsed events.
pub struct BufferedStream {
    events: Vec<StreamEvent>,
    pos: usize,
}

impl BufferedStream {
    pub fn new(events: Vec<StreamEvent>) -> Self {
        Self { events, pos: 0 }
    }
}

impl EventStream for BufferedStream {
    fn next_event(&mut self) -> Option<StreamEvent> {
        if self.pos < self.events.len() {
            let event = self.events[self.pos].clone();
            self.pos += 1;
            Some(event)
        } else {
            None
        }
    }
}

struct PreloadedTransport {
    stream: RefCell<Option<BufferedStream>>,
}

impl Transport for PreloadedTransport {
    type Stream = BufferedStream;

    fn send(&self, _request: CompletionRequest) -> Result<Self::Stream, String> {
        self.stream
            .borrow_mut()
            .take()
            .ok_or_else(|| "stream already consumed".into())
    }
}

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

fn parse_sse_events(body: &str) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    for line in body.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            events.push(StreamEvent::MessageStop);
            continue;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "content_block_start" => {
                if let Some(cb) = json.get("content_block") {
                    let cb_type = cb.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if cb_type == "tool_use" {
                        let id = cb
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = cb
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        events.push(StreamEvent::ToolUseStart { id, name });
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = json.get("delta") {
                    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::TextDelta(text.to_string()));
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|t| t.as_str())
                            {
                                events.push(StreamEvent::ToolUseInputDelta(partial.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_stop" => {
                events.push(StreamEvent::MessageStop);
            }
            "error" => {
                let msg = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                events.push(StreamEvent::Error(msg.to_string()));
            }
            _ => {
                // ping, message_start, content_block_stop, message_delta — ignore
            }
        }
    }

    events
}

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
    api_key: String,
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

        let mut context = Namespace::new();
        context.mount(
            "system",
            Box::new(SystemProvider::new(system_prompt.to_string())),
        );
        context.mount("history", Box::new(HistoryProvider::new()));
        context.mount("tools", Box::new(ToolsProvider::new(schemas)));
        context.mount("model", Box::new(ModelProvider::new(model, max_tokens)));

        Self {
            api_key: api_key.to_string(),
            context: Rc::new(RefCell::new(context)),
            event_callback: None,
            rust_tools: Rc::new(RefCell::new(tool_registry)),
            js_tools: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Update the API key used for Anthropic API requests.
    pub fn set_api_key(&mut self, api_key: &str) {
        self.api_key = api_key.to_string();
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
            .read(&path!("model/id"))
            .ok()
            .flatten()
            .map(record_to_json);
        let model_max_tokens = ctx
            .read(&path!("model/max_tokens"))
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

        let model_catalog = ctx
            .read(&path!("model/catalog"))
            .ok()
            .flatten()
            .map(record_to_json);

        let snapshot = serde_json::json!({
            "system": system,
            "model": {
                "id": model_id,
                "max_tokens": model_max_tokens,
                "catalog": model_catalog,
            },
            "tools": tools,
            "history": {
                "count": history_count,
                "messages": history_messages,
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
            .write(&path!("model/id"), record)
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
        let catalog = ctx
            .read(&path!("model/catalog"))
            .ok()
            .flatten()
            .map(record_to_json)
            .unwrap_or(serde_json::Value::Array(vec![]));
        catalog.to_string()
    }

    /// Fetch available models from the Anthropic API and write to the catalog.
    /// Returns a Promise that resolves with the JSON catalog array.
    pub fn refresh_models(&self) -> js_sys::Promise {
        let api_key = self.api_key.clone();
        let context = self.context.clone();
        let callback = self.event_callback.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let models = fetch_model_catalog(&api_key).await?;
            let value = structfs_serde_store::to_value(&models)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            context
                .borrow_mut()
                .write(&path!("model/catalog"), Record::parsed(value))
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
        let api_key = self.api_key.clone();
        let callback = self.event_callback.clone();
        let context = self.context.clone();
        let rust_tools = self.rust_tools.clone();
        let js_tools = self.js_tools.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let result = run_agentic_loop(
                &api_key,
                &input,
                &context,
                &rust_tools,
                &js_tools,
                callback.as_ref(),
            )
            .await;

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
        let schemas = {
            let mut schemas = self.rust_tools.borrow().schemas();
            for jt in self.js_tools.borrow().values() {
                schemas.push(jt.schema());
            }
            schemas
        };
        self.context
            .borrow_mut()
            .mount("tools", Box::new(ToolsProvider::new(schemas)));
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

async fn fetch_completion(api_key: &str, request_body: &str) -> Result<String, String> {
    let window = web_sys::window().ok_or("no window")?;

    let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| format!("{e:?}"))?;
    headers
        .set("x-api-key", api_key)
        .map_err(|e| format!("{e:?}"))?;
    headers
        .set("anthropic-version", "2023-06-01")
        .map_err(|e| format!("{e:?}"))?;
    headers
        .set("anthropic-dangerous-direct-browser-access", "true")
        .map_err(|e| format!("{e:?}"))?;

    let url = "https://api.anthropic.com/v1/messages";

    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(web_sys::RequestMode::Cors);
    opts.set_body(&JsValue::from_str(request_body));
    opts.set_headers(&headers);

    let request =
        web_sys::Request::new_with_str_and_init(url, &opts).map_err(|e| format!("{e:?}"))?;

    let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| {
            let msg = js_sys::Reflect::get(&e, &"message".into())
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            if msg.contains("Failed to fetch") {
                "Could not reach api.anthropic.com — check your network connection".to_string()
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
            401 => Err("Invalid API key — check your Anthropic API key and try again".to_string()),
            _ => Err(format!("HTTP {status}: {body}")),
        };
    }

    let text = wasm_bindgen_futures::JsFuture::from(resp.text().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("{e:?}"))?;

    text.as_string()
        .ok_or_else(|| "response body not a string".into())
}

/// Fetch the model catalog from the Anthropic API.
async fn fetch_model_catalog(api_key: &str) -> Result<Vec<ModelInfo>, JsValue> {
    let window = web_sys::window().ok_or(JsValue::from_str("no window"))?;

    let headers = web_sys::Headers::new().map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
    headers
        .set("x-api-key", api_key)
        .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
    headers
        .set("anthropic-version", "2023-06-01")
        .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
    headers
        .set("anthropic-dangerous-direct-browser-access", "true")
        .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;

    let mut all_models: Vec<ModelInfo> = Vec::new();
    let mut after_id: Option<String> = None;

    loop {
        let url = match &after_id {
            Some(cursor) => {
                format!("https://api.anthropic.com/v1/models?limit=1000&after_id={cursor}")
            }
            None => "https://api.anthropic.com/v1/models?limit=1000".to_string(),
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
                all_models.push(ModelInfo { id, display_name });
            }
        }

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
    api_key: &str,
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

    loop {
        // Read the prompt from the namespace
        let prompt_record = context_ref
            .borrow_mut()
            .read(&path!("prompt"))
            .map_err(|e| e.to_string())?
            .ok_or("failed to read prompt from context")?;
        let prompt_json = match prompt_record {
            Record::Parsed(v) => value_to_json(v),
            _ => return Err("expected parsed prompt record".into()),
        };
        let request: CompletionRequest =
            serde_json::from_value(prompt_json).map_err(|e| e.to_string())?;

        let request_body = serde_json::to_string(&request).map_err(|e| e.to_string())?;

        emit_js(callback, "turn_start", "");
        emit_js(callback, "request_sent", &request_body);

        let response_body = fetch_completion(api_key, &request_body).await?;
        let events = parse_sse_events(&response_body);

        // Use Kernel::stream_once to accumulate the SSE events into content
        // blocks. We drive the tool-call loop here (not inside the kernel)
        // because each round requires an async fetch.
        let stream = BufferedStream::new(events);
        let preloaded = PreloadedTransport {
            stream: RefCell::new(Some(stream)),
        };

        let model_id = {
            let record = context_ref
                .borrow_mut()
                .read(&path!("model/id"))
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

        let content = kernel.stream_once(request, &preloaded, &mut emit)?;

        // Extract tool calls
        let tool_calls: Vec<ox_kernel::ToolCall> = content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::ToolUse(tc) = b {
                    Some(tc.clone())
                } else {
                    None
                }
            })
            .collect();

        // Write assistant message to the namespace
        let assistant_json = serialize_assistant_message(&content);
        let record = Record::parsed(json_to_value(assistant_json));
        context_ref
            .borrow_mut()
            .write(&path!("history/append"), record)
            .map_err(|e| e.to_string())?;
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

        // Execute tools and write results to the namespace
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

        let results_json = serialize_tool_results(&results);
        let record = Record::parsed(json_to_value(results_json));
        context_ref
            .borrow_mut()
            .write(&path!("history/append"), record)
            .map_err(|e| e.to_string())?;
        emit_js(callback, "context_changed", "");

        // Loop back for next completion
    }
}

/// Create an agent with direct Anthropic API access. Exported to JS.
#[wasm_bindgen]
pub fn create_agent(system_prompt: &str, api_key: &str) -> OxAgent {
    OxAgent::new(system_prompt, api_key)
}
