//! Encode internal StreamEvents into wire SSE (dialect-aware).
//!
//! Sans-IO: holds per-stream dialect state (OpenAI's tool-call counter,
//! Anthropic's content_block index, etc.) and emits zero or more wire
//! frames per event. Each frame is a complete `event: <name>\ndata: <json>\n\n`
//! block (Anthropic) or `data: <json>\n\n` block (OpenAI); the caller
//! writes each into the SSE response body.

use crate::codec::ResponseMeta;
use ox_types::StreamEvent;
use std::collections::HashMap;

pub struct SseEncoder {
    dialect: String,
    meta: ResponseMeta,
    // Set once any tool call starts; drives stop_reason/finish_reason.
    saw_tool_use: bool,
    // Anthropic state
    next_content_block: usize,
    open_text_block: Option<usize>,
    open_tool_block: Option<usize>,
    // OpenAI state
    openai_message_started: bool,
    openai_tool_index: HashMap<String, u32>,
}

impl SseEncoder {
    pub fn new(dialect: &str, meta: ResponseMeta) -> Self {
        Self {
            dialect: dialect.to_string(),
            meta,
            saw_tool_use: false,
            next_content_block: 0,
            open_text_block: None,
            open_tool_block: None,
            openai_message_started: false,
            openai_tool_index: HashMap::new(),
        }
    }

    /// Encode one event into zero or more complete wire SSE frames.
    /// Caller writes each frame into the response body verbatim.
    pub fn encode_sse(&mut self, event: &StreamEvent) -> Vec<String> {
        match self.dialect.as_str() {
            "anthropic" => self.encode_anthropic(event),
            "openai" => self.encode_openai(event),
            _ => vec![],
        }
    }

    /// Final closing frame the dialect requires. `data: [DONE]\n\n` for
    /// OpenAI; nothing for Anthropic (message_stop already closes the stream).
    pub fn finish(&mut self) -> Vec<String> {
        match self.dialect.as_str() {
            "openai" => vec!["data: [DONE]\n\n".into()],
            _ => vec![],
        }
    }

    fn encode_anthropic(&mut self, event: &StreamEvent) -> Vec<String> {
        match event {
            StreamEvent::InputUsage { input_tokens, cache_creation, cache_read } => {
                let frame = serde_json::json!({
                    "type": "message_start",
                    "message": {
                        "id": self.meta.id,
                        "type": "message",
                        "role": "assistant",
                        "model": self.meta.model,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": input_tokens,
                            "cache_creation_input_tokens": cache_creation,
                            "cache_read_input_tokens": cache_read,
                            "output_tokens": 0,
                        },
                    },
                });
                vec![format!("event: message_start\ndata: {}\n\n", frame)]
            }
            StreamEvent::TextDelta { text } => {
                let mut out = Vec::new();
                if self.open_text_block.is_none() {
                    let idx = self.next_content_block;
                    self.next_content_block += 1;
                    self.open_text_block = Some(idx);
                    out.push(format!(
                        "event: content_block_start\ndata: {}\n\n",
                        serde_json::json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": { "type": "text", "text": "" },
                        })
                    ));
                }
                let idx = self.open_text_block.unwrap();
                out.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "text_delta", "text": text },
                    })
                ));
                out
            }
            StreamEvent::ToolUseStart { id, name } => {
                self.saw_tool_use = true;
                let mut out = Vec::new();
                if let Some(text_idx) = self.open_text_block.take() {
                    out.push(format!(
                        "event: content_block_stop\ndata: {}\n\n",
                        serde_json::json!({ "type": "content_block_stop", "index": text_idx })
                    ));
                }
                let idx = self.next_content_block;
                self.next_content_block += 1;
                self.open_tool_block = Some(idx);
                out.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} },
                    })
                ));
                out
            }
            StreamEvent::ToolUseInputDelta { delta } => {
                let idx = match self.open_tool_block {
                    Some(i) => i,
                    None => return vec![],
                };
                vec![format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "input_json_delta", "partial_json": delta },
                    })
                )]
            }
            StreamEvent::OutputUsage { output_tokens } => {
                let stop_reason = if self.saw_tool_use { "tool_use" } else { "end_turn" };
                vec![format!(
                    "event: message_delta\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "message_delta",
                        "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                        "usage": { "output_tokens": output_tokens },
                    })
                )]
            }
            StreamEvent::MessageStop => {
                let mut out = Vec::new();
                if let Some(idx) = self.open_text_block.take() {
                    out.push(format!(
                        "event: content_block_stop\ndata: {}\n\n",
                        serde_json::json!({ "type": "content_block_stop", "index": idx })
                    ));
                }
                if let Some(idx) = self.open_tool_block.take() {
                    out.push(format!(
                        "event: content_block_stop\ndata: {}\n\n",
                        serde_json::json!({ "type": "content_block_stop", "index": idx })
                    ));
                }
                out.push(format!(
                    "event: message_stop\ndata: {}\n\n",
                    serde_json::json!({ "type": "message_stop" })
                ));
                out
            }
            StreamEvent::Error { message } => {
                vec![format!(
                    "event: error\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "error",
                        "error": { "type": "api_error", "message": message },
                    })
                )]
            }
        }
    }

    fn encode_openai(&mut self, event: &StreamEvent) -> Vec<String> {
        let (id, model, created) =
            (self.meta.id.clone(), self.meta.model.clone(), self.meta.created);
        let chunk = |delta: serde_json::Value, finish_reason: Option<&str>| {
            serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish_reason,
                }],
            })
        };
        let (id2, model2) = (self.meta.id.clone(), self.meta.model.clone());
        let usage_chunk = move |usage: serde_json::Value| {
            serde_json::json!({
                "id": id2,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model2,
                "choices": [],
                "usage": usage,
            })
        };

        match event {
            StreamEvent::TextDelta { text } => {
                let mut delta = serde_json::json!({ "content": text });
                if !self.openai_message_started {
                    delta["role"] = serde_json::Value::String("assistant".into());
                    self.openai_message_started = true;
                }
                vec![format!("data: {}\n\n", chunk(delta, None))]
            }
            StreamEvent::ToolUseStart { id, name } => {
                self.saw_tool_use = true;
                let next = self.openai_tool_index.len() as u32;
                self.openai_tool_index.insert(id.clone(), next);
                let delta = serde_json::json!({
                    "tool_calls": [{
                        "index": next,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": "" },
                    }]
                });
                vec![format!("data: {}\n\n", chunk(delta, None))]
            }
            StreamEvent::ToolUseInputDelta { delta } => {
                let idx = self.openai_tool_index.len().saturating_sub(1) as u32;
                let payload = serde_json::json!({
                    "tool_calls": [{ "index": idx, "function": { "arguments": delta } }]
                });
                vec![format!("data: {}\n\n", chunk(payload, None))]
            }
            StreamEvent::MessageStop => {
                let finish = if self.saw_tool_use { "tool_calls" } else { "stop" };
                vec![format!("data: {}\n\n", chunk(serde_json::json!({}), Some(finish)))]
            }
            StreamEvent::InputUsage { input_tokens, cache_read, .. } => {
                vec![format!(
                    "data: {}\n\n",
                    usage_chunk(serde_json::json!({
                        "prompt_tokens": input_tokens,
                        "prompt_tokens_details": { "cached_tokens": cache_read },
                    }))
                )]
            }
            StreamEvent::OutputUsage { output_tokens } => {
                vec![format!(
                    "data: {}\n\n",
                    usage_chunk(serde_json::json!({ "completion_tokens": output_tokens }))
                )]
            }
            StreamEvent::Error { message } => {
                vec![format!(
                    "data: {}\n\n",
                    serde_json::json!({ "error": { "type": "api_error", "message": message } })
                )]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> ResponseMeta {
        ResponseMeta {
            id: "msg_test01".into(),
            model: "claude-test".into(),
            created: 1_700_000_000,
        }
    }

    #[test]
    fn anthropic_text_delta_emits_block_start_then_delta() {
        let mut enc = SseEncoder::new("anthropic", meta());
        let frames = enc.encode_sse(&StreamEvent::TextDelta { text: "hi".into() });
        assert_eq!(frames.len(), 2);
        assert!(frames[0].contains("content_block_start"));
        assert!(frames[1].contains("content_block_delta"));
        assert!(frames[1].contains("\"text\":\"hi\""));
    }

    #[test]
    fn anthropic_second_text_delta_reuses_block() {
        let mut enc = SseEncoder::new("anthropic", meta());
        let _ = enc.encode_sse(&StreamEvent::TextDelta { text: "a".into() });
        let frames = enc.encode_sse(&StreamEvent::TextDelta { text: "b".into() });
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("\"text\":\"b\""));
    }

    #[test]
    fn anthropic_message_stop_closes_open_block() {
        let mut enc = SseEncoder::new("anthropic", meta());
        let _ = enc.encode_sse(&StreamEvent::TextDelta { text: "x".into() });
        let frames = enc.encode_sse(&StreamEvent::MessageStop);
        assert_eq!(frames.len(), 2);
        assert!(frames[0].contains("content_block_stop"));
        assert!(frames[1].contains("message_stop"));
    }

    #[test]
    fn anthropic_tool_use_closes_open_text_block() {
        let mut enc = SseEncoder::new("anthropic", meta());
        let _ = enc.encode_sse(&StreamEvent::TextDelta { text: "a".into() });
        let frames = enc.encode_sse(&StreamEvent::ToolUseStart {
            id: "t1".into(),
            name: "x".into(),
        });
        // Expect text block close + tool block start
        assert!(frames.iter().any(|f| f.contains("content_block_stop")));
        assert!(frames.iter().any(|f| f.contains("content_block_start") && f.contains("tool_use")));
    }

    #[test]
    fn anthropic_message_start_carries_id_and_model() {
        let mut enc = SseEncoder::new("anthropic", meta());
        let frames = enc.encode_sse(&StreamEvent::InputUsage {
            input_tokens: 100,
            cache_creation: 0,
            cache_read: 0,
        });
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("message_start"));
        assert!(frames[0].contains("\"id\":\"msg_test01\""));
        assert!(frames[0].contains("\"model\":\"claude-test\""));
        assert!(frames[0].contains("\"input_tokens\":100"));
    }

    #[test]
    fn anthropic_stop_reason_reflects_tool_use() {
        let mut enc = SseEncoder::new("anthropic", meta());
        let _ = enc.encode_sse(&StreamEvent::ToolUseStart { id: "t1".into(), name: "x".into() });
        let frames = enc.encode_sse(&StreamEvent::OutputUsage { output_tokens: 5 });
        assert!(frames[0].contains("\"stop_reason\":\"tool_use\""));
    }

    #[test]
    fn anthropic_stop_reason_end_turn_without_tools() {
        let mut enc = SseEncoder::new("anthropic", meta());
        let frames = enc.encode_sse(&StreamEvent::OutputUsage { output_tokens: 5 });
        assert!(frames[0].contains("\"stop_reason\":\"end_turn\""));
    }

    #[test]
    fn openai_first_text_includes_role() {
        let mut enc = SseEncoder::new("openai", meta());
        let frames = enc.encode_sse(&StreamEvent::TextDelta { text: "hi".into() });
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("\"role\":\"assistant\""));
        assert!(frames[0].contains("\"content\":\"hi\""));
    }

    #[test]
    fn openai_second_text_omits_role() {
        let mut enc = SseEncoder::new("openai", meta());
        let _ = enc.encode_sse(&StreamEvent::TextDelta { text: "a".into() });
        let frames = enc.encode_sse(&StreamEvent::TextDelta { text: "b".into() });
        assert_eq!(frames.len(), 1);
        assert!(!frames[0].contains("\"role\""));
    }

    #[test]
    fn openai_chunks_carry_meta() {
        let mut enc = SseEncoder::new("openai", meta());
        let frames = enc.encode_sse(&StreamEvent::TextDelta { text: "hi".into() });
        assert!(frames[0].contains("\"id\":\"msg_test01\""));
        assert!(frames[0].contains("\"model\":\"claude-test\""));
        assert!(frames[0].contains("\"created\":1700000000"));
    }

    #[test]
    fn openai_finish_emits_done() {
        let mut enc = SseEncoder::new("openai", meta());
        let frames = enc.finish();
        assert_eq!(frames, vec!["data: [DONE]\n\n"]);
    }

    #[test]
    fn anthropic_finish_emits_nothing() {
        let mut enc = SseEncoder::new("anthropic", meta());
        let frames = enc.finish();
        assert!(frames.is_empty());
    }

    #[test]
    fn openai_message_stop_emits_stop_finish_reason() {
        let mut enc = SseEncoder::new("openai", meta());
        let frames = enc.encode_sse(&StreamEvent::MessageStop);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("\"finish_reason\":\"stop\""));
    }

    #[test]
    fn openai_message_stop_after_tools_emits_tool_calls() {
        let mut enc = SseEncoder::new("openai", meta());
        let _ = enc.encode_sse(&StreamEvent::ToolUseStart { id: "t1".into(), name: "x".into() });
        let frames = enc.encode_sse(&StreamEvent::MessageStop);
        assert!(frames[0].contains("\"finish_reason\":\"tool_calls\""));
    }
}
