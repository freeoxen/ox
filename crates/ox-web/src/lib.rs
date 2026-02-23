use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
use ox_core::{
    AgentEvent, CompletionRequest, ContentBlock, EventStream, StreamEvent, Transport,
    serialize_assistant_message, serialize_tool_results,
};
use ox_history::HistoryProvider;
use ox_kernel::{Reader, Record, ToolResult, Value, Writer, path};
use std::cell::RefCell;
use std::rc::Rc;
use structfs_serde_store::{json_to_value, value_to_json};
use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// ProxyTransport — calls the dev server via browser fetch (synchronous pull)
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

// ---------------------------------------------------------------------------
// PreloadedTransport — wraps a pre-fetched BufferedStream
// ---------------------------------------------------------------------------

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
// Wasm-bindgen exports
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct OxAgent {
    server_url: String,
    context: Rc<RefCell<Namespace>>,
    event_callback: Option<js_sys::Function>,
}

#[wasm_bindgen]
impl OxAgent {
    #[wasm_bindgen(constructor)]
    pub fn new(system_prompt: &str, server_url: &str) -> Self {
        let model = "claude-sonnet-4-20250514".to_string();
        let max_tokens = 4096;

        // Build tool schemas snapshot for the namespace
        let mut tool_registry = ox_kernel::ToolRegistry::new();
        tool_registry.register(Box::new(ox_kernel::ReverseTextTool));
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
            server_url: server_url.to_string(),
            context: Rc::new(RefCell::new(context)),
            event_callback: None,
        }
    }

    /// Register a JS callback to receive agent events.
    pub fn on_event(&mut self, callback: js_sys::Function) {
        self.event_callback = Some(callback);
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

    /// Send a user prompt and run the agentic loop to completion.
    /// Returns a Promise that resolves with the final assistant text.
    pub fn prompt(&self, input: &str) -> js_sys::Promise {
        let input = input.to_string();
        let server_url = self.server_url.clone();
        let callback = self.event_callback.clone();
        let context = self.context.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let result = run_agentic_loop(&server_url, &input, &context, callback.as_ref()).await;

            match result {
                Ok(text) => Ok(JsValue::from_str(&text)),
                Err(e) => Err(JsValue::from_str(&e)),
            }
        })
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

async fn fetch_completion(server_url: &str, request_body: &str) -> Result<String, String> {
    let window = web_sys::window().ok_or("no window")?;

    let headers = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| format!("{e:?}"))?;

    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(web_sys::RequestMode::Cors);
    opts.set_body(&JsValue::from_str(request_body));
    opts.set_headers(&headers);

    let url = format!("{server_url}/complete");
    let request =
        web_sys::Request::new_with_str_and_init(&url, &opts).map_err(|e| format!("{e:?}"))?;

    let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("{e:?}"))?;

    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|_| "response is not a Response".to_string())?;

    if !resp.ok() {
        let status = resp.status();
        let text = wasm_bindgen_futures::JsFuture::from(resp.text().map_err(|e| format!("{e:?}"))?)
            .await
            .map_err(|e| format!("{e:?}"))?;
        return Err(format!(
            "HTTP {status}: {}",
            text.as_string().unwrap_or_default()
        ));
    }

    let text = wasm_bindgen_futures::JsFuture::from(resp.text().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("{e:?}"))?;

    text.as_string()
        .ok_or_else(|| "response body not a string".into())
}

/// Run the full agentic loop: prompt -> stream -> tool call -> result -> loop.
async fn run_agentic_loop(
    server_url: &str,
    user_input: &str,
    context_ref: &Rc<RefCell<Namespace>>,
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

    let tools = {
        let mut registry = ox_kernel::ToolRegistry::new();
        registry.register(Box::new(ox_kernel::ReverseTextTool));
        registry
    };

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

        let response_body = fetch_completion(server_url, &request_body).await?;
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

        // Execute tools locally and write results to the namespace
        let mut results = Vec::new();
        for tc in &tool_calls {
            emit_js(callback, "tool_call_start", &tc.name);
            let result_str = match tools.get(&tc.name) {
                Some(tool) => match tool.execute(tc.input.clone()) {
                    Ok(r) => r,
                    Err(e) => format!("error: {e}"),
                },
                None => format!("error: unknown tool '{}'", tc.name),
            };
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

/// Create an agent. Convenience function exported to JS.
#[wasm_bindgen]
pub fn create_agent(system_prompt: &str, server_url: &str) -> OxAgent {
    OxAgent::new(system_prompt, server_url)
}
