use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteManagerError {
    #[error("invalid remote request: {0}")]
    Invalid(String),
    #[error("remote operation unavailable: {0}")]
    Unavailable(String),
    #[error("remote identity mismatch: {0}")]
    IdentityMismatch(String),
    #[error("required node is unavailable: {0}")]
    RequiredNodeUnavailable(String),
    #[error("remote operation is leased by another reconciler")]
    LeaseHeld,
    #[error("node deletion refused; active remote references: {0:?}")]
    ActiveReferences(Vec<String>),
    #[error("injected crash after {0}")]
    InjectedCrash(&'static str),
    #[error("StructFS {operation} failed: {message}")]
    Store {
        operation: &'static str,
        message: String,
    },
}

impl RemoteManagerError {
    pub(crate) fn store(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Store {
            operation,
            message: error.to_string(),
        }
    }
}

impl From<structfs_core_store::PathError> for RemoteManagerError {
    fn from(value: structfs_core_store::PathError) -> Self {
        Self::store("path", value)
    }
}
