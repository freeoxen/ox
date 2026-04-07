//! Async store traits — broker-internal, not a StructFS primitive.

use std::future::Future;
use std::pin::Pin;
use structfs_core_store::{Error as StoreError, Path, Record};

/// A boxed, Send, 'static future.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Async version of structfs Reader.
pub trait AsyncReader: Send + 'static {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>>;
}

/// Async version of structfs Writer. The returned future is 'static + Send —
/// it does NOT borrow the store. The store produces the future synchronously
/// (&mut self), the future resolves asynchronously (detached).
pub trait AsyncWriter: Send + 'static {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>>;
}
