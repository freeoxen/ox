//! Minimal tracing subscriber for the Wasm guest.
//!
//! Routes log events through the host bridge by writing JSON to
//! `events/log`. The host's HostStore can intercept this path and
//! forward to the CLI's tracing subscriber.

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};

/// A tracing subscriber that writes log events through the Wasm host bridge.
pub struct WasmSubscriber;

impl Subscriber for WasmSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= &Level::DEBUG
    }

    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let level = meta.level().as_str();
        let target = meta.target();

        let mut visitor = MessageVisitor {
            message: String::new(),
            fields: String::new(),
        };
        event.record(&mut visitor);

        let line = if visitor.fields.is_empty() {
            format!("{level} {target}: {}", visitor.message)
        } else {
            format!(
                "{level} {target}: {}{} {}",
                visitor.message,
                if visitor.message.is_empty() { "" } else { "," },
                visitor.fields
            )
        };

        // Write as JSON string through host bridge
        if let Ok(json) = serde_json::to_string(&line) {
            let _ = super::host_write("events/log", &json);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn core::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            self.fields
                .push_str(&format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            self.fields
                .push_str(&format!("{}=\"{}\"", field.name(), value));
        }
    }
}
