use ox_core::{AgentEvent, CompletionRequest, EventStream, StreamEvent, Transport};
use std::cell::RefCell;
use std::rc::Rc;
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
    system_prompt: String,
    model: String,
    max_tokens: u32,
    history_messages: Rc<RefCell<Vec<serde_json::Value>>>,
    tool_schemas: Vec<ox_kernel::ToolSchema>,
    event_callback: Option<js_sys::Function>,
}

#[wasm_bindgen]
impl OxAgent {
    #[wasm_bindgen(constructor)]
    pub fn new(system_prompt: &str, server_url: &str) -> Self {
        let model = "claude-sonnet-4-20250514".to_string();
        let max_tokens = 4096;
        let mut tools = ox_core::ToolRegistry::new();
        tools.register(Box::new(ox_kernel::ReverseTextTool));
        let tool_schemas = tools.schemas();

        Self {
            server_url: server_url.to_string(),
            system_prompt: system_prompt.to_string(),
            model,
            max_tokens,
            history_messages: Rc::new(RefCell::new(Vec::new())),
            tool_schemas,
            event_callback: None,
        }
    }

    /// Register a JS callback to receive agent events.
    pub fn on_event(&mut self, callback: js_sys::Function) {
        self.event_callback = Some(callback);
    }

    /// Send a user prompt and run the agentic loop to completion.
    /// Returns a Promise that resolves with the final assistant text.
    pub fn prompt(&self, input: &str) -> js_sys::Promise {
        let input = input.to_string();
        let server_url = self.server_url.clone();
        let system_prompt = self.system_prompt.clone();
        let model = self.model.clone();
        let max_tokens = self.max_tokens;
        let tool_schemas = self.tool_schemas.clone();
        let callback = self.event_callback.clone();
        let history_messages = self.history_messages.clone();

        wasm_bindgen_futures::future_to_promise(async move {
            let cfg = LoopConfig {
                server_url: &server_url,
                system_prompt: &system_prompt,
                model: &model,
                max_tokens,
                tool_schemas,
                callback: callback.as_ref(),
            };
            let result = run_agentic_loop(&cfg, &input, &history_messages).await;

            match result {
                Ok(text) => Ok(JsValue::from_str(&text)),
                Err(e) => Err(JsValue::from_str(&e)),
            }
        })
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

struct LoopConfig<'a> {
    server_url: &'a str,
    system_prompt: &'a str,
    model: &'a str,
    max_tokens: u32,
    tool_schemas: Vec<ox_kernel::ToolSchema>,
    callback: Option<&'a js_sys::Function>,
}

/// Run the full agentic loop: prompt -> stream -> tool call -> result -> loop.
async fn run_agentic_loop(
    cfg: &LoopConfig<'_>,
    user_input: &str,
    history_ref: &Rc<RefCell<Vec<serde_json::Value>>>,
) -> Result<String, String> {
    // Add the user message to history
    history_ref.borrow_mut().push(serde_json::json!({
        "role": "user",
        "content": user_input,
    }));

    let tools = {
        let mut registry = ox_core::ToolRegistry::new();
        registry.register(Box::new(ox_kernel::ReverseTextTool));
        registry
    };

    loop {
        let messages = history_ref.borrow().clone();
        let request = ox_kernel::CompletionRequest {
            model: cfg.model.to_string(),
            max_tokens: cfg.max_tokens,
            system: cfg.system_prompt.to_string(),
            messages,
            tools: cfg.tool_schemas.clone(),
            stream: true,
        };

        let request_body = serde_json::to_string(&request).map_err(|e| e.to_string())?;

        emit_js(cfg.callback, "turn_start", "");

        let response_body = fetch_completion(cfg.server_url, &request_body).await?;
        let events = parse_sse_events(&response_body);

        // Use Kernel::stream_once to accumulate the SSE events into content
        // blocks. We drive the tool-call loop here (not inside the kernel)
        // because each round requires an async fetch.
        let stream = BufferedStream::new(events);
        let preloaded = PreloadedTransport {
            stream: RefCell::new(Some(stream)),
        };

        let mut kernel = ox_kernel::Kernel::new(cfg.model.to_string());
        let accumulate_request = ox_kernel::CompletionRequest {
            model: cfg.model.to_string(),
            max_tokens: cfg.max_tokens,
            system: cfg.system_prompt.to_string(),
            messages: history_ref.borrow().clone(),
            tools: cfg.tool_schemas.clone(),
            stream: true,
        };

        let mut emit = |event: AgentEvent| match &event {
            AgentEvent::TextDelta(text) => emit_js(cfg.callback, "text_delta", text),
            AgentEvent::ToolCallStart { name } => emit_js(cfg.callback, "tool_call_start", name),
            AgentEvent::ToolCallResult { name, result } => emit_js(
                cfg.callback,
                "tool_call_result",
                &format!("{name}: {result}"),
            ),
            AgentEvent::TurnEnd => emit_js(cfg.callback, "turn_end", ""),
            AgentEvent::Error(e) => emit_js(cfg.callback, "error", e),
            _ => {}
        };

        let content = kernel.stream_once(accumulate_request, &preloaded, &mut emit)?;

        // Extract tool calls
        let tool_calls: Vec<ox_kernel::ToolCall> = content
            .iter()
            .filter_map(|b| {
                if let ox_core::ContentBlock::ToolUse(tc) = b {
                    Some(tc.clone())
                } else {
                    None
                }
            })
            .collect();

        // Append assistant message to history
        let assistant_content: Vec<serde_json::Value> = content
            .iter()
            .map(|b| match b {
                ox_core::ContentBlock::Text { text } => serde_json::json!({
                    "type": "text",
                    "text": text,
                }),
                ox_core::ContentBlock::ToolUse(tc) => serde_json::json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": tc.input,
                }),
            })
            .collect();

        history_ref.borrow_mut().push(serde_json::json!({
            "role": "assistant",
            "content": assistant_content,
        }));

        if tool_calls.is_empty() {
            let text = content
                .iter()
                .filter_map(|b| {
                    if let ox_core::ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");

            emit_js(cfg.callback, "turn_end", "");
            return Ok(text);
        }

        // Execute tools locally and append results
        let mut tool_results = Vec::new();
        for tc in &tool_calls {
            emit_js(cfg.callback, "tool_call_start", &tc.name);
            let result_str = match tools.get(&tc.name) {
                Some(tool) => match tool.execute(tc.input.clone()) {
                    Ok(r) => r,
                    Err(e) => format!("error: {e}"),
                },
                None => format!("error: unknown tool '{}'", tc.name),
            };
            emit_js(
                cfg.callback,
                "tool_call_result",
                &format!("{}: {}", tc.name, result_str),
            );
            tool_results.push(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tc.id,
                "content": result_str,
            }));
        }

        history_ref.borrow_mut().push(serde_json::json!({
            "role": "user",
            "content": tool_results,
        }));

        // Loop back for next completion
    }
}

/// Create an agent. Convenience function exported to JS.
#[wasm_bindgen]
pub fn create_agent(system_prompt: &str, server_url: &str) -> OxAgent {
    OxAgent::new(system_prompt, server_url)
}
