//! Test-only `SseHttpExecutor` that yields a scripted event sequence
//! with optional inter-event delays. Used by completion_broker lifecycle
//! tests and by ox-gateway integration tests.

#![cfg(any(test, feature = "test-utils"))]

use crate::transport::SseHttpExecutor;
use futures::stream::BoxStream;
use ox_types::StreamEvent;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use structfs_http::types::HttpRequest;

/// A scripted-sequence executor. Push (delay, Result) entries before
/// the consumer triggers a request; on `execute`, the script is drained
/// in order, sleeping `delay` before each item.
///
/// Also records every `HttpRequest` it sees, so tests can assert what
/// the broker built and sent.
pub struct MockSseExecutor {
    script: Mutex<Vec<(Duration, Result<StreamEvent, String>)>>,
    requests_seen: Arc<Mutex<Vec<HttpRequest>>>,
}

impl MockSseExecutor {
    pub fn new() -> Self {
        Self {
            script: Mutex::new(Vec::new()),
            requests_seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Push one (delay, event-or-error) to the script.
    pub fn push(&self, delay: Duration, item: Result<StreamEvent, String>) {
        self.script.lock().unwrap().push((delay, item));
    }

    /// Convenience: push an event with no delay.
    pub fn push_immediate(&self, event: StreamEvent) {
        self.push(Duration::ZERO, Ok(event));
    }

    /// Push an error with no delay (terminates the stream).
    pub fn push_error(&self, message: impl Into<String>) {
        self.push(Duration::ZERO, Err(message.into()));
    }

    /// Inspect what requests have hit the executor. Useful when a test
    /// wants to assert the broker's resolved URL / headers / body.
    pub fn requests_seen(&self) -> Vec<HttpRequest> {
        self.requests_seen.lock().unwrap().clone()
    }
}

impl Default for MockSseExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SseHttpExecutor for MockSseExecutor {
    async fn execute(
        &self,
        request: HttpRequest,
        _dialect: String,
    ) -> BoxStream<'static, Result<StreamEvent, String>> {
        self.requests_seen.lock().unwrap().push(request);
        let script: Vec<_> = std::mem::take(&mut *self.script.lock().unwrap());
        Box::pin(async_stream::stream! {
            for (delay, item) in script {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                yield item;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn yields_scripted_events_in_order() {
        let exec = MockSseExecutor::new();
        exec.push_immediate(StreamEvent::TextDelta { text: "a".into() });
        exec.push_immediate(StreamEvent::TextDelta { text: "b".into() });
        exec.push_immediate(StreamEvent::MessageStop);

        let mut stream = exec.execute(HttpRequest::default(), "anthropic".into()).await;
        let mut texts = Vec::new();
        while let Some(item) = stream.next().await {
            match item.unwrap() {
                StreamEvent::TextDelta { text } => texts.push(text),
                StreamEvent::MessageStop => break,
                _ => {}
            }
        }
        assert_eq!(texts, vec!["a".to_string(), "b".to_string()]);
    }

    #[tokio::test]
    async fn errors_propagate() {
        let exec = MockSseExecutor::new();
        exec.push_error("boom");
        let mut stream = exec.execute(HttpRequest::default(), "anthropic".into()).await;
        let first = stream.next().await.unwrap();
        assert!(first.is_err());
        assert_eq!(first.unwrap_err(), "boom");
    }

    #[tokio::test]
    async fn records_request() {
        let exec = MockSseExecutor::new();
        let req = HttpRequest::post("https://example.com/v1/messages");
        let _ = exec.execute(req.clone(), "anthropic".into()).await;
        let seen = exec.requests_seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path, "https://example.com/v1/messages");
    }
}
