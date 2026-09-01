//! Reusable execution core shared by interactive and headless ox hosts.
//!
//! This crate owns the existing Wasm agent pool and its tool, policy,
//! approval, token-accounting, and snapshot behavior. Surfaces provide the
//! broker namespace and presentation; they do not implement a second runtime.

mod agents;
mod broker_mounts;
mod clash_sandbox;
mod commit_drain;
mod ingress;
mod policy;
mod policy_check;
pub mod test_support;
mod thread_registry;

pub use agents::{
    AgentPool, ExecutionCommandError, ExecutionCore, ExecutionHandle, ExecutorConfig,
    IngressBoundary, IngressFailpoints, PolicyProfile, SYSTEM_PROMPT, ThreadExecutionConfig,
    write_save_result_to_inbox,
};
pub use broker_mounts::mount_execution_stores;
pub use policy::{CheckResult, PolicyGuard, PolicyLoadError, PolicyStats};
pub use thread_registry::{
    LEDGER_HEALTH_DEGRADED, LEDGER_HEALTH_MISSING, LEDGER_HEALTH_OK, LEDGER_HEALTH_REPAIR_FAILED,
    ThreadNamespace, ThreadRegistry,
};

/// Existing shell contract used by the crash-recovery Skip decision.
pub const POST_CRASH_SKIP_CONTENT: &str = "[ox-cli: skipped by user after crash recovery. \
    The tool was not re-executed. Do not retry this tool in this turn.]";
