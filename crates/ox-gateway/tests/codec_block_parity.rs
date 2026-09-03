//! Golden parity: the wasm codec Block must produce byte-identical output
//! to the native codecs for every op, across both dialects. This is the
//! phase-1 gate for the Isotope migration — the same source compiled to
//! wasm32 and driven through the store ABI proves the Block boundary
//! before any routing moves onto it.

use ox_codec::ResponseMeta;
use ox_gateway::codec_block;
use ox_types::StreamEvent;

fn meta() -> ResponseMeta {
    ResponseMeta {
        id: "msg_parity01".into(),
        model: "claude-parity".into(),
        created: 1_700_000_000,
    }
}

fn meta_json() -> serde_json::Value {
    serde_json::json!({ "id": "msg_parity01", "model": "claude-parity", "created": 1_700_000_000 })
}

fn event_corpus() -> Vec<StreamEvent> {
    vec![
        StreamEvent::InputUsage {
            input_tokens: 12,
            cache_creation: 3,
            cache_read: 4,
        },
        StreamEvent::TextDelta {
            text: "Thinking about it. ".into(),
        },
        StreamEvent::ToolUseStart {
            id: "toolu_1".into(),
            name: "read_file".into(),
        },
        StreamEvent::ToolUseInputDelta {
            delta: r#"{"path":"#.into(),
        },
        StreamEvent::ToolUseInputDelta {
            delta: r#""/etc/hosts"}"#.into(),
        },
        StreamEvent::ToolUseStart {
            id: "toolu_2".into(),
            name: "grep".into(),
        },
        StreamEvent::ToolUseInputDelta {
            delta: r#"{"q":"π ünïcode"}"#.into(),
        },
        StreamEvent::OutputUsage { output_tokens: 9 },
        StreamEvent::MessageStop,
    ]
}

fn request_corpus() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "anthropic",
            serde_json::json!({
                "model": "anthropic/claude-sonnet-4-20250514",
                "max_tokens": 128,
                "system": [{"type": "text", "text": "be brief"}, {"type": "text", "text": "be kind"}],
                "messages": [{"role": "user", "content": "hej π"}],
                "tools": [{"name": "read_file", "description": "reads", "input_schema": {"type": "object"}}],
                "temperature": 0.2,
                "top_k": 40,
                "stop_sequences": ["END"],
                "tool_choice": {"type": "any"},
                "stream": true
            }),
        ),
        (
            "openai",
            serde_json::json!({
                "model": "openai/gpt-4o",
                "max_completion_tokens": 256,
                "messages": [
                    {"role": "system", "content": "sys prompt"},
                    {"role": "user", "content": "hello"}
                ],
                "tools": [{"type": "function", "function": {"name": "grep", "description": "d", "parameters": {"type": "object"}}}],
                "temperature": 1.5,
                "stop": "DONE",
                "tool_choice": "required",
                "seed": 7
            }),
        ),
    ]
}

#[test]
fn decode_request_parity() {
    for (dialect, body) in request_corpus() {
        let native = match dialect {
            "openai" => ox_codec::openai::decode_request(&body),
            _ => ox_codec::anthropic::decode_request(&body),
        }
        .expect("native decode");
        let native_json = serde_json::to_value(&native).unwrap();

        let block = codec_block::run_job(serde_json::json!({
            "op": "decode_request",
            "dialect": dialect,
            "body": body,
        }))
        .expect("block decode");

        assert_eq!(native_json, block, "decode_request diverged for {dialect}");
    }
}

#[test]
fn decode_request_error_parity() {
    let bad = serde_json::json!({ "max_tokens": 5, "messages": [] });
    let native_err = ox_codec::anthropic::decode_request(&bad)
        .unwrap_err()
        .to_string();
    let block_err = codec_block::run_job(serde_json::json!({
        "op": "decode_request",
        "dialect": "anthropic",
        "body": bad,
    }))
    .unwrap_err();
    assert_eq!(native_err, block_err);
}

#[test]
fn encode_response_parity() {
    let events = event_corpus();
    let events_json = serde_json::to_value(&events).unwrap();
    for dialect in ["anthropic", "openai"] {
        let native = match dialect {
            "openai" => ox_codec::openai::encode_response(&events, &meta()),
            _ => ox_codec::anthropic::encode_response(&events, &meta()),
        };
        let block = codec_block::run_job(serde_json::json!({
            "op": "encode_response",
            "dialect": dialect,
            "events": events_json,
            "meta": meta_json(),
        }))
        .expect("block encode_response");
        assert_eq!(native, block, "encode_response diverged for {dialect}");
    }
}

#[test]
fn encode_stream_parity() {
    let events = event_corpus();
    let events_json = serde_json::to_value(&events).unwrap();
    for dialect in ["anthropic", "openai"] {
        let mut enc = ox_codec::SseEncoder::new(dialect, meta());
        let mut native_frames: Vec<String> = Vec::new();
        for ev in &events {
            native_frames.extend(enc.encode_sse(ev));
        }
        let native_finish = enc.finish();

        let block = codec_block::run_job(serde_json::json!({
            "op": "encode_stream",
            "dialect": dialect,
            "events": events_json,
            "meta": meta_json(),
        }))
        .expect("block encode_stream");
        let block_frames: Vec<String> = serde_json::from_value(block["frames"].clone()).unwrap();
        let block_finish: Vec<String> = serde_json::from_value(block["finish"].clone()).unwrap();

        assert_eq!(
            native_frames, block_frames,
            "stream frames diverged for {dialect}"
        );
        assert_eq!(
            native_finish, block_finish,
            "finish frames diverged for {dialect}"
        );
    }
}

#[test]
fn translate_request_parity() {
    for (dialect, body) in request_corpus() {
        let decoded = match dialect {
            "openai" => ox_codec::openai::decode_request(&body),
            _ => ox_codec::anthropic::decode_request(&body),
        }
        .unwrap();
        // Cross-dialect too: each decoded request through both translators.
        for upstream in ["anthropic", "openai"] {
            let native = match upstream {
                "openai" => ox_codec::openai::translate_request(&decoded),
                _ => ox_codec::anthropic::translate_request(&decoded),
            };
            let block = codec_block::run_job(serde_json::json!({
                "op": "translate_request",
                "dialect": upstream,
                "request": serde_json::to_value(&decoded).unwrap(),
            }))
            .expect("block translate");
            assert_eq!(
                native, block,
                "translate_request diverged: inbound {dialect} → upstream {upstream}"
            );
        }
    }
}
