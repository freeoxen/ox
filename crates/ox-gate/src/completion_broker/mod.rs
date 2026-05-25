//! `CompletionBrokerStore` — substrate-mediated LLM completion dispatch,
//! modeled on structfs_http::HttpBrokerStore but generalized for streaming.
//!
//! Path layout (mirrors HttpBrokerStore's `outstanding/{N}` convention):
//!   write /                                CompletionRequest → outstanding/{N}
//!   read  outstanding/{N}                  CompletionStatus
//!   read  outstanding/{N}/request          original CompletionRequest
//!   read  outstanding/{N}/events/from/{S}  Vec<StreamEvent> — BLOCKING
//!   read  outstanding/{N}/events/count     usize — non-blocking buffer length
//!   read  outstanding/{N}/usage            UsageInfo (None until Complete)
//!   write outstanding/{N} null             GC

mod dispatch;
mod inflight;

pub use inflight::CompletionStatus;
#[allow(unused_imports)]
pub(crate) use inflight::{Inflight, InflightState};

use std::collections::HashMap;
use std::sync::Arc;

use ox_broker::ClientHandle;
use structfs_core_store::Path;
use tokio::runtime::Handle as TokioHandle;

use crate::transport::SseHttpExecutor;

pub type RequestId = u64;

/// Streaming completion broker — Reader/Writer impls come in Tasks 3.4
/// and 3.5. Generic on the executor for mockability (same pattern as
/// structfs_http::HttpBrokerStore<E: HttpExecutor>).
pub struct CompletionBrokerStore<E: SseHttpExecutor> {
    /// Broker client handle used by per-request dispatch tasks to resolve
    /// gate/* and secret/* paths. Cloned per spawn.
    #[allow(dead_code)]
    pub(crate) substrate: ClientHandle,

    /// Upstream streaming HTTP executor. Held as Arc so each per-request
    /// task can clone cheaply.
    #[allow(dead_code)]
    pub(crate) executor: Arc<E>,

    /// In-memory in-flight tracker. Per-request state has its own Notify.
    /// No outer Mutex needed — AsyncReader/AsyncWriter give us &mut self.
    #[allow(dead_code)]
    pub(crate) handles: HashMap<RequestId, Arc<Inflight>>,

    #[allow(dead_code)]
    pub(crate) next_request_id: RequestId,

    /// Broker client scoped to gateway/usage for appending UsageRecords on
    /// Complete (Task 3.4 uses this).
    #[allow(dead_code)]
    pub(crate) usage_writer: ClientHandle,

    /// Tokio handle for spawning per-request dispatch tasks.
    #[allow(dead_code)]
    pub(crate) runtime: TokioHandle,
}

impl<E: SseHttpExecutor> CompletionBrokerStore<E> {
    pub fn new(
        substrate: ClientHandle,
        executor: Arc<E>,
        usage_writer: ClientHandle,
        runtime: TokioHandle,
    ) -> Self {
        Self {
            substrate,
            executor,
            handles: HashMap::new(),
            next_request_id: 0,
            usage_writer,
            runtime,
        }
    }

    /// Parse `outstanding/{id}[/sub/...]` path. Returns the request id
    /// and any sub-path as a `/`-joined string.
    pub(crate) fn parse_handle_path(path: &Path) -> Option<(RequestId, Option<String>)> {
        if path.is_empty() || path[0].as_str() != "outstanding" {
            return None;
        }
        if path.len() == 1 {
            return None;
        }
        let id: RequestId = path[1].as_str().parse().ok()?;
        let sub = if path.len() > 2 {
            Some(
                (2..path.len())
                    .map(|i| path[i].as_str())
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        } else {
            None
        };
        Some((id, sub))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn parse_handle_path_basic() {
        assert_eq!(
            CompletionBrokerStore::<crate::transport::ReqwestSseExecutor>::parse_handle_path(
                &path!("outstanding/42")
            ),
            Some((42, None))
        );
    }

    #[test]
    fn parse_handle_path_with_subpath() {
        assert_eq!(
            CompletionBrokerStore::<crate::transport::ReqwestSseExecutor>::parse_handle_path(
                &path!("outstanding/7/events/from/3")
            ),
            Some((7, Some("events/from/3".into())))
        );
    }

    #[test]
    fn parse_handle_path_root_returns_none() {
        assert_eq!(
            CompletionBrokerStore::<crate::transport::ReqwestSseExecutor>::parse_handle_path(
                &path!("")
            ),
            None
        );
    }

    #[test]
    fn parse_handle_path_outstanding_only_returns_none() {
        assert_eq!(
            CompletionBrokerStore::<crate::transport::ReqwestSseExecutor>::parse_handle_path(
                &path!("outstanding")
            ),
            None
        );
    }

    #[test]
    fn parse_handle_path_nonnumeric_id_returns_none() {
        assert_eq!(
            CompletionBrokerStore::<crate::transport::ReqwestSseExecutor>::parse_handle_path(
                &path!("outstanding/abc")
            ),
            None
        );
    }
}
