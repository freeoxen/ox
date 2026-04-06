//! Request types for the broker protocol.
//!
//! Each request carries its own reply channel, making the response path
//! compile-time verifiable. The server holding a Request has everything
//! it needs to respond — no broker round-trip required.

use structfs_core_store::{Error as StoreError, Path, Record};
use tokio::sync::oneshot;

/// A request routed through the broker from client to server.
///
/// The reply channel is embedded in each variant, so the server responds
/// directly without going back through the broker's state machine.
pub enum Request {
    Read {
        /// Path relative to the server's mount prefix.
        path: Path,
        /// One-shot channel for the server to send its response.
        reply: oneshot::Sender<Result<Option<Record>, StoreError>>,
    },
    Write {
        /// Path relative to the server's mount prefix.
        path: Path,
        /// The data to write.
        data: Record,
        /// One-shot channel for the server to send its response.
        reply: oneshot::Sender<Result<Path, StoreError>>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{path, Value};

    #[test]
    fn read_request_carries_reply() {
        let (tx, _rx) = oneshot::channel();
        let req = Request::Read {
            path: path!("history/messages"),
            reply: tx,
        };
        assert!(matches!(req, Request::Read { .. }));
    }

    #[test]
    fn write_request_carries_reply() {
        let (tx, _rx) = oneshot::channel();
        let req = Request::Write {
            path: path!("history/append"),
            data: Record::parsed(Value::String("hello".to_string())),
            reply: tx,
        };
        assert!(matches!(req, Request::Write { .. }));
    }

    #[test]
    fn read_reply_delivers_result() {
        let (tx, rx) = oneshot::channel::<Result<Option<Record>, StoreError>>();
        let record = Record::parsed(Value::Integer(42));
        tx.send(Ok(Some(record))).unwrap();
        let result = rx.blocking_recv().unwrap();
        assert!(matches!(result, Ok(Some(_))));
    }

    #[test]
    fn write_reply_delivers_result() {
        let (tx, rx) = oneshot::channel::<Result<Path, StoreError>>();
        tx.send(Ok(path!("result/path"))).unwrap();
        let result = rx.blocking_recv().unwrap();
        assert!(matches!(result, Ok(_)));
    }

    #[test]
    fn write_reply_delivers_error() {
        let (tx, rx) = oneshot::channel::<Result<Path, StoreError>>();
        tx.send(Err(StoreError::store("test", "write", "failed")))
            .unwrap();
        let result = rx.blocking_recv().unwrap();
        assert!(matches!(result, Err(_)));
    }
}
