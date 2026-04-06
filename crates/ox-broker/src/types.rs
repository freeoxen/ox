//! Request/Response types for the broker protocol.
//!
//! Adapted from appiware's broker_store.rs but using ox's StructFS types
//! (Record, Value, Path, StoreError) instead of generic serde types.

use structfs_core_store::{Error as StoreError, Path, Record};

/// A request routed through the broker from client to server.
#[derive(Debug)]
pub struct Request {
    /// Unique action ID for matching responses.
    pub action_id: u64,
    /// The operation to perform.
    pub kind: RequestKind,
    /// Path relative to the server's mount prefix.
    pub path: Path,
}

/// The kind of operation requested.
#[derive(Debug)]
pub enum RequestKind {
    Read,
    Write(Record),
}

/// A response from a server back to the waiting client.
#[derive(Debug)]
pub enum Response {
    Read(Result<Option<Record>, StoreError>),
    Write(Result<Path, StoreError>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Value, path};

    #[test]
    fn request_read_construction() {
        let req = Request {
            action_id: 1,
            kind: RequestKind::Read,
            path: path!("history/messages"),
        };
        assert_eq!(req.action_id, 1);
        assert!(matches!(req.kind, RequestKind::Read));
    }

    #[test]
    fn request_write_construction() {
        let record = Record::parsed(Value::String("hello".to_string()));
        let req = Request {
            action_id: 2,
            kind: RequestKind::Write(record),
            path: path!("history/append"),
        };
        assert_eq!(req.action_id, 2);
        assert!(matches!(req.kind, RequestKind::Write(_)));
    }

    #[test]
    fn response_read_ok() {
        let resp = Response::Read(Ok(Some(Record::parsed(Value::Integer(42)))));
        assert!(matches!(resp, Response::Read(Ok(Some(_)))));
    }

    #[test]
    fn response_read_none() {
        let resp = Response::Read(Ok(None));
        assert!(matches!(resp, Response::Read(Ok(None))));
    }

    #[test]
    fn response_write_ok() {
        let resp = Response::Write(Ok(path!("result/path")));
        assert!(matches!(resp, Response::Write(Ok(_))));
    }

    #[test]
    fn response_write_err() {
        let resp = Response::Write(Err(StoreError::store("test", "write", "failed")));
        assert!(matches!(resp, Response::Write(Err(_))));
    }
}
