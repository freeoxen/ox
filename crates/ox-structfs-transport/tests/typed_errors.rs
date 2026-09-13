//! StructFS 0.2 error categories survive the existing wire v1 carrier.

use ox_broker::async_store::BoxFuture;
use ox_structfs_transport::{ExportRoot, RemoteError, WireErrorCode, connect_in_process};
use structfs_core_store::{DetachedReader, DetachedWriter, Error, Path, Record, Value};

struct FailureStore;

fn failure(path: &Path) -> Error {
    match path.iter().last().unwrap() {
        "missing" => Error::not_found(path.clone()),
        "denied" => Error::permission_denied("restricted"),
        "conflict" => Error::conflict("revision changed"),
        "overloaded" => Error::overloaded("busy"),
        "deadline" => Error::deadline_exceeded("expired"),
        "limit" => Error::resource_limit("too large"),
        _ => panic!("unknown test case"),
    }
}

impl DetachedReader for FailureStore {
    fn read_detached(&mut self, path: &Path) -> BoxFuture<Result<Option<Record>, Error>> {
        Box::pin(std::future::ready(Err(failure(path))))
    }
}

impl DetachedWriter for FailureStore {
    fn write_detached(&mut self, path: &Path, _: Record) -> BoxFuture<Result<Path, Error>> {
        Box::pin(std::future::ready(Err(failure(path))))
    }
}

#[tokio::test]
async fn typed_categories_survive_reads_and_writes_without_a_custom_mapper() {
    let mut remote = connect_in_process(
        ExportRoot::new(FailureStore, Path::parse("private_root").unwrap()),
        Default::default(),
        Default::default(),
        4096,
    );
    for (name, code) in [
        ("missing", WireErrorCode::NotFound),
        ("denied", WireErrorCode::PermissionDenied),
        ("conflict", WireErrorCode::Conflict),
        ("overloaded", WireErrorCode::Overloaded),
        ("deadline", WireErrorCode::DeadlineExceeded),
        ("limit", WireErrorCode::ResourceLimit),
    ] {
        let path = Path::parse(name).unwrap();
        assert!(matches!(remote.read_remote(&path).await,
            Err(RemoteError::Wire { code: actual, .. }) if actual == code));

        let error = structfs_core_store::AsyncReader::read_async(&mut remote, &path)
            .await
            .unwrap_err();
        let expected = failure(&path);
        assert_eq!(
            std::mem::discriminant(&error),
            std::mem::discriminant(&expected)
        );
        if let Error::NotFound { path: returned } = error {
            assert_eq!(returned, path, "typed paths stay in the caller namespace");
        }

        let error = DetachedWriter::write_detached(&mut remote, &path, Record::parsed(Value::Null))
            .await
            .unwrap_err();
        assert_eq!(
            std::mem::discriminant(&error),
            std::mem::discriminant(&expected)
        );
    }
}
