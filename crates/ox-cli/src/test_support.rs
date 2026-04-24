//! Test-support helpers for the crash harness.
//!
//! Always compiled in (no feature gate) because `tests/crash_harness/` is an
//! integration test and sees only the `pub` surface of the `ox-cli` library.
//! The cost to the `ox` binary is one unused struct — trivial.
//!
//! Nothing in here is consumed by the production binary.

use std::sync::{Arc, Mutex};

use ox_kernel::{CompletionRequest, StreamEvent};
use ox_tools::completion::{CompletionOutput, CompletionTransport};

/// A scripted sequence of stream events, replayed one completion at a time.
///
/// Each call to [`CompletionTransport::send`] consumes one "turn" of scripted
/// events — a contiguous run terminated by [`StreamEvent::TurnEnd`]. After the
/// events for the current turn have been emitted to the caller, `send` returns
/// the accumulated vector plus token counts.
///
/// The harness records call count so Task 3's approval-resume tests can assert
/// the transport was not re-invoked after a crash.
#[derive(Clone)]
pub struct FakeTransport {
    inner: Arc<Mutex<FakeTransportInner>>,
}

struct FakeTransportInner {
    /// Events to replay, grouped by turn (inner Vec = one turn).
    turns: Vec<Vec<StreamEvent>>,
    /// Turns consumed so far.
    cursor: usize,
    /// Total call count — assertable via [`FakeTransport::call_count`].
    calls: usize,
    /// If set, `send` returns an error when `calls` would exceed `max_calls`.
    max_calls: Option<usize>,
    /// If set, `send` returns an error when `calls` reaches `fail_at`.
    fail_at: Option<usize>,
    /// If set, token counts returned per call.
    input_tokens: u32,
    output_tokens: u32,
}

impl FakeTransport {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeTransportInner {
                turns: Vec::new(),
                cursor: 0,
                calls: 0,
                max_calls: None,
                fail_at: None,
                input_tokens: 0,
                output_tokens: 0,
            })),
        }
    }

    /// Append a turn's worth of events. The last event is typically a
    /// `StreamEvent::MessageStop` or similar terminator — `send` returns
    /// when the script for one turn is exhausted.
    pub fn push_turn(&self, events: Vec<StreamEvent>) -> &Self {
        self.inner.lock().unwrap().turns.push(events);
        self
    }

    /// Cap the total number of calls. `send` returns `Err` on the call that
    /// would exceed this cap.
    pub fn fail_if_called_more_than(&self, n: usize) -> &Self {
        self.inner.lock().unwrap().max_calls = Some(n);
        self
    }

    /// Inject a token-usage pair returned by every call.
    pub fn with_token_usage(&self, input: u32, output: u32) -> &Self {
        let mut inner = self.inner.lock().unwrap();
        inner.input_tokens = input;
        inner.output_tokens = output;
        self
    }

    /// Total number of `send` invocations observed.
    pub fn call_count(&self) -> usize {
        self.inner.lock().unwrap().calls
    }

    /// Number of scripted turns remaining.
    pub fn turns_remaining(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.turns.len().saturating_sub(inner.cursor)
    }
}

impl Default for FakeTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionTransport for FakeTransport {
    fn send(
        &self,
        _request: &CompletionRequest,
        on_event: &dyn Fn(&StreamEvent),
    ) -> Result<CompletionOutput, String> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls += 1;

        if let Some(cap) = inner.max_calls {
            if inner.calls > cap {
                return Err(format!(
                    "FakeTransport: called {} times, cap is {}",
                    inner.calls, cap
                ));
            }
        }
        if Some(inner.calls) == inner.fail_at {
            return Err("FakeTransport: scripted failure".to_string());
        }

        let idx = inner.cursor;
        if idx >= inner.turns.len() {
            return Err(format!(
                "FakeTransport: script exhausted after {} turn(s)",
                inner.turns.len()
            ));
        }
        inner.cursor += 1;
        let events = inner.turns[idx].clone();
        let input_tokens = inner.input_tokens;
        let output_tokens = inner.output_tokens;
        drop(inner);

        for event in &events {
            on_event(event);
        }

        Ok(CompletionOutput {
            events,
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        })
    }
}

/// Factory used by `AgentPool::new_with_transport_factory`. The closure is
/// invoked once per spawned worker, yielding a fresh boxed transport.
pub type TransportFactory = Arc<dyn Fn() -> Box<dyn CompletionTransport> + Send + Sync>;

/// Wrap a cloneable transport as a factory. Each call to the factory returns a
/// boxed clone, satisfying `CompletionModule::set_transport`'s ownership model
/// while leaving the original handle addressable from the test for assertions.
pub fn factory_for<T>(transport: T) -> TransportFactory
where
    T: CompletionTransport + Clone + 'static,
{
    Arc::new(move || Box::new(transport.clone()))
}

/// Test-only tool injector — registered tools are inserted into each
/// spawned worker's `ToolStore` before the worker services its first
/// turn. Task 3d's post-crash-reconfirm E2E tests use this to wire a
/// counter-incrementing tool whose side effect can be asserted across a
/// crash/remount cycle.
///
/// The factory is invoked once per worker (via `Arc::clone` + call),
/// yielding a fresh `Vec` of boxed `NativeTool`s. Each tool is
/// installed via `ToolStore::register_native`.
pub type ToolInjector = Arc<dyn Fn() -> Vec<Box<dyn ox_tools::native::NativeTool>> + Send + Sync>;
