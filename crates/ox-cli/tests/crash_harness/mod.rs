//! Crash-test harness.
//!
//! Two modes:
//! - **In-process "soft crash"** — build an `App`, drive it, drop it, rebuild
//!   against the same `$HOME/.ox`. The kind of crash that lives at the app
//!   layer: a panic, a clean `Ctrl+C` shutdown, a dropped tokio runtime.
//! - **Subprocess `SIGKILL`** — spawn the real `ox` binary against a temp
//!   `$HOME/.ox`, signal it, verify the exit status, then remount in-process
//!   for assertions.
//!
//! The harness asserts on two layers only:
//! - The ledger JSONL bytes on disk.
//! - The in-memory `SharedLog` projection (read through the broker).
//!
//! Nothing here speaks `ratatui` or terminal output. A correct `SharedLog`
//! round-trip implies a correct UI render by construction (the UI is a
//! deterministic function of the log).
//!
//! Task 0 scope (this file): lifecycle + assertion scaffolding, plus a
//! `FakeTransport` wired through `App::new_with_transport_factory`. The
//! `LedgerWriter` freeze hook referenced in the plan's Step 5 is a no-op
//! today; it becomes load-bearing when Task 1a introduces `LedgerWriter`.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ox_broker::{BrokerStore, ClientHandle};
use ox_cli::app::App;
use ox_cli::broker_setup::{BrokerHandle, setup as broker_setup};
use ox_cli::test_support::{FakeTransport, TransportFactory, factory_for};
use ox_inbox::InboxStore;
use ox_kernel::log::LogEntry;
use structfs_core_store::{Record, Value, path};
use tempfile::TempDir;

/// Wire a subscriber that echoes `RUST_LOG` to stderr. A no-op if a subscriber
/// is already installed (e.g. by another test in the same process).
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_test_writer()
        .try_init();
}

// ---------------------------------------------------------------------------
// HarnessBuilder
// ---------------------------------------------------------------------------

/// Build a self-contained ox-cli harness rooted at a private temp dir.
pub struct HarnessBuilder {
    inbox_root: Option<TempDir>,
    workspace: Option<TempDir>,
    fake_transport: Option<FakeTransport>,
    extra_config: BTreeMap<String, Value>,
}

impl HarnessBuilder {
    pub fn new() -> Self {
        Self {
            inbox_root: None,
            workspace: None,
            fake_transport: None,
            extra_config: BTreeMap::new(),
        }
    }

    /// Use a specific `FakeTransport` (shared with the caller so tests can
    /// assert `call_count()` / script more turns).
    pub fn with_transport(mut self, transport: FakeTransport) -> Self {
        self.fake_transport = Some(transport);
        self
    }

    /// Inject an arbitrary config key/value. Used for tests that need to
    /// override the default model, account, or similar.
    pub fn with_config(mut self, key: impl Into<String>, value: Value) -> Self {
        self.extra_config.insert(key.into(), value);
        self
    }

    /// Finalize — build the broker, mount stores, construct the `App`.
    pub async fn build(self) -> Harness {
        let inbox_root_dir = self.inbox_root.unwrap_or_else(|| {
            tempfile::Builder::new()
                .prefix("ox-crash-harness-inbox-")
                .tempdir()
                .expect("create inbox temp dir")
        });
        let workspace_dir = self.workspace.unwrap_or_else(|| {
            tempfile::Builder::new()
                .prefix("ox-crash-harness-workspace-")
                .tempdir()
                .expect("create workspace temp dir")
        });
        let inbox_root = inbox_root_dir.path().to_path_buf();
        let workspace = workspace_dir.path().to_path_buf();

        let mut config = default_test_config();
        config.extend(self.extra_config);

        let broker_handle = build_broker(&inbox_root, config).await;
        let broker = broker_handle.broker.clone();

        let fake_transport = self.fake_transport.unwrap_or_default();

        let factory: TransportFactory = factory_for(fake_transport.clone());

        let app = App::new_with_transport_factory(
            workspace.clone(),
            inbox_root.clone(),
            /* no_policy */ true,
            broker.clone(),
            tokio::runtime::Handle::current(),
            Some(factory),
        )
        .expect("construct App");

        Harness {
            app: Some(app),
            broker_handle: Some(broker_handle),
            inbox_root,
            workspace,
            _inbox_root_dir: inbox_root_dir,
            _workspace_dir: workspace_dir,
            fake_transport,
        }
    }
}

impl Default for HarnessBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A live in-process harness owning an `App`, its broker, and the backing
/// temp directories.
pub struct Harness {
    app: Option<App>,
    broker_handle: Option<BrokerHandle>,
    inbox_root: PathBuf,
    workspace: PathBuf,
    // TempDir handles must outlive the harness; they clean up on drop.
    _inbox_root_dir: TempDir,
    _workspace_dir: TempDir,
    fake_transport: FakeTransport,
}

impl Harness {
    pub fn app(&mut self) -> &mut App {
        self.app.as_mut().expect("App dropped; harness is dead")
    }

    pub fn broker(&self) -> &BrokerStore {
        &self.broker_handle.as_ref().expect("broker dropped").broker
    }

    pub fn client(&self) -> ClientHandle {
        self.broker_handle
            .as_ref()
            .expect("broker dropped")
            .client()
    }

    pub fn inbox_root(&self) -> &Path {
        &self.inbox_root
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn fake_transport(&self) -> &FakeTransport {
        &self.fake_transport
    }

    /// Capture the full ordered list of `LogEntry`s for `thread_id` through
    /// the broker. Used to snapshot pre-crash state and compare to the
    /// post-remount state.
    pub async fn snapshot_shared_log(&self, thread_id: &str) -> Vec<LogEntry> {
        let client = self.client();
        read_shared_log(&client, thread_id).await
    }

    /// Simulate a soft crash: drop the `App` and the broker. Any background
    /// threads the App owns (AgentPool workers) receive closed channels and
    /// exit naturally. The temp dir persists so the same state can be
    /// re-opened by `remount_app`.
    ///
    /// Step 2.5 of the plan asks us to audit `App::drop`. Today `App` has no
    /// explicit `Drop` impl (verified); field-by-field drop is enough because
    /// the AgentPool's worker threads exit when their `prompt_rx` closes —
    /// which happens as `AgentPool` drops its `threads` map.
    pub fn soft_crash(&mut self) {
        // Drop App first so worker threads see closed channels.
        self.app.take();
        // Then tear down broker servers.
        self.broker_handle.take();
    }

    /// Rebuild the harness against the same temp dirs. Used after
    /// [`Harness::soft_crash`] to exercise the remount path.
    ///
    /// NOTE: `fake_transport` starts fresh. Tests that care about post-crash
    /// transport call counts should pass their own `FakeTransport` into
    /// `HarnessBuilder::with_transport` and re-use it here.
    pub async fn remount_app(&mut self) {
        assert!(self.app.is_none(), "soft_crash must precede remount_app");
        assert!(self.broker_handle.is_none(), "broker leaked past crash");

        let config = default_test_config();
        let broker_handle = build_broker(&self.inbox_root, config).await;
        let broker = broker_handle.broker.clone();
        let factory: TransportFactory = factory_for(self.fake_transport.clone());

        let app = App::new_with_transport_factory(
            self.workspace.clone(),
            self.inbox_root.clone(),
            /* no_policy */ true,
            broker,
            tokio::runtime::Handle::current(),
            Some(factory),
        )
        .expect("remount App");

        self.app = Some(app);
        self.broker_handle = Some(broker_handle);
    }

    /// Path to the thread directory on disk: `$HOME/.ox/threads/{tid}`.
    pub fn thread_dir(&self, thread_id: &str) -> PathBuf {
        self.inbox_root.join("threads").join(thread_id)
    }

    /// Path to the ledger JSONL for `thread_id`.
    pub fn ledger_path(&self, thread_id: &str) -> PathBuf {
        self.thread_dir(thread_id).join("ledger.jsonl")
    }
}

// ---------------------------------------------------------------------------
// Broker / config plumbing
// ---------------------------------------------------------------------------

fn default_test_config() -> BTreeMap<String, Value> {
    let mut cfg = BTreeMap::new();
    cfg.insert(
        "gate/defaults/model".into(),
        Value::String("claude-sonnet-4-20250514".into()),
    );
    cfg.insert(
        "gate/defaults/account".into(),
        Value::String("anthropic".into()),
    );
    cfg.insert("gate/defaults/max_tokens".into(), Value::Integer(4096));
    cfg.insert(
        "gate/accounts/anthropic/provider".into(),
        Value::String("anthropic".into()),
    );
    cfg.insert(
        "gate/accounts/anthropic/key".into(),
        Value::String("fake-harness-key".into()),
    );
    cfg
}

async fn build_broker(inbox_root: &Path, config: BTreeMap<String, Value>) -> BrokerHandle {
    let inbox = InboxStore::open(inbox_root).expect("open inbox store");
    let bindings = ox_cli::bindings::default_bindings();
    broker_setup(inbox, bindings, inbox_root.to_path_buf(), config).await
}

// ---------------------------------------------------------------------------
// Read helpers
// ---------------------------------------------------------------------------

/// Read the full log for `thread_id` from the broker as `Vec<LogEntry>`.
///
/// Mirrors the pattern production uses to render history: `threads/{tid}/log/entries`.
pub async fn read_shared_log(client: &ClientHandle, thread_id: &str) -> Vec<LogEntry> {
    let tid = ox_kernel::PathComponent::try_new(thread_id).expect("valid thread id");
    let log_path = ox_path::oxpath!("threads", tid, "log", "entries");
    let Some(record) = client.read(&log_path).await.expect("read log/entries") else {
        return Vec::new();
    };
    let Some(value) = record.as_value() else {
        return Vec::new();
    };
    let json = structfs_serde_store::value_to_json(value.clone());
    let Some(arr) = json.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .cloned()
        .map(|v| serde_json::from_value::<LogEntry>(v).expect("parse LogEntry"))
        .collect()
}

/// Append a `LogEntry` to a thread's structured log via the broker.
pub async fn append_log_entry(client: &ClientHandle, thread_id: &str, entry: LogEntry) {
    let tid = ox_kernel::PathComponent::try_new(thread_id).expect("valid thread id");
    let log_path = ox_path::oxpath!("threads", tid, "log", "append");
    client
        .write_typed(&log_path, &entry)
        .await
        .expect("log/append");
}

/// Create a thread through the inbox and return its generated id.
pub async fn create_thread(client: &ClientHandle, title: &str) -> String {
    let mut create = BTreeMap::new();
    create.insert("title".to_string(), Value::String(title.into()));
    let created = client
        .write(&path!("inbox/threads"), Record::parsed(Value::Map(create)))
        .await
        .expect("create thread");
    created
        .components
        .last()
        .expect("new thread path carries id")
        .as_str()
        .to_string()
}

// ---------------------------------------------------------------------------
// Ledger helpers — read .jsonl directly. Useful for assertions that want to
// see the on-disk bytes (not the in-memory projection).
// ---------------------------------------------------------------------------

pub fn read_ledger_entries(ledger_path: &Path) -> Vec<ox_inbox::ledger::LedgerEntry> {
    ox_inbox::ledger::read_ledger(ledger_path).expect("read ledger")
}

pub fn ledger_exists(ledger_path: &Path) -> bool {
    ledger_path.exists()
}

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

pub fn assert_ledger_entries_eq(ledger_path: &Path, expected: &[LogEntry]) {
    let entries = read_ledger_entries(ledger_path);
    let actual: Vec<LogEntry> = entries
        .into_iter()
        .map(|e| serde_json::from_value::<LogEntry>(e.msg).expect("ledger msg parses"))
        .collect();
    assert_eq!(
        actual.len(),
        expected.len(),
        "ledger length mismatch: got {}, want {}",
        actual.len(),
        expected.len()
    );
    for (i, (got, want)) in actual.iter().zip(expected.iter()).enumerate() {
        let got_json = serde_json::to_value(got).unwrap();
        let want_json = serde_json::to_value(want).unwrap();
        assert_eq!(
            got_json, want_json,
            "ledger entry {i} mismatch: got={got_json:?} want={want_json:?}"
        );
    }
}

pub fn assert_shared_log_matches_pre_kill(actual: &[LogEntry], pre_kill: &[LogEntry]) {
    let a = entries_as_json(actual);
    let p = entries_as_json(pre_kill);
    assert_eq!(
        a.len(),
        p.len(),
        "SharedLog length mismatch after remount: got {}, pre-kill was {}",
        a.len(),
        p.len()
    );
    for (i, (g, w)) in a.iter().zip(p.iter()).enumerate() {
        assert_eq!(
            g, w,
            "SharedLog entry {i} differs after remount: got={g:?} pre-kill was={w:?}"
        );
    }
}

pub fn assert_no_dangling_turn_start(entries: &[LogEntry]) {
    // A dangling TurnStart has no matching TurnEnd later.
    let mut open: i32 = 0;
    let mut last_open: Option<usize> = None;
    for (i, e) in entries.iter().enumerate() {
        match e {
            LogEntry::TurnStart { .. } => {
                open += 1;
                last_open = Some(i);
            }
            LogEntry::TurnEnd { .. } => {
                if open > 0 {
                    open -= 1;
                }
            }
            _ => {}
        }
    }
    assert!(
        open == 0,
        "dangling TurnStart at index {:?} (open count {open})",
        last_open
    );
}

pub fn assert_transport_called_exactly(transport: &FakeTransport, expected: usize) {
    let got = transport.call_count();
    assert_eq!(
        got, expected,
        "FakeTransport called {got} times; expected {expected}"
    );
}

fn entries_as_json(entries: &[LogEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|e| serde_json::to_value(e).expect("serialize"))
        .collect()
}

// ---------------------------------------------------------------------------
// Waiting helpers — intentionally absent.
//
// A previous draft had a `wait_for_turn_settled` that polled the broker with
// `tokio::time::sleep`. Wall-clock polling is the wrong primitive for tests:
// flaky, slow, and it encodes a bug (the turn may finish before the first
// poll). When Task 3+ actually needs to wait on a turn, the right shape is a
// broker subscription or a `oneshot` signaled by the worker on turn
// completion. Build that when it's needed — don't ship a sleep-loop today.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Subprocess mode (minimal — Task 0 Step 4).
//
// Task 0 requires a smoke test that spawns ox-cli as a subprocess and asserts
// it can be SIGKILL'd cleanly. Driving a scripted transport *in* the
// subprocess is left to later tasks — the env-var protocol is defined here
// so Task 1's torn-tail tests can grow it.
// ---------------------------------------------------------------------------

/// Configuration for a subprocess-mode run.
pub struct SubprocessHarness {
    pub inbox_root: PathBuf,
    pub workspace: PathBuf,
    _inbox_root_dir: TempDir,
    _workspace_dir: TempDir,
}

impl SubprocessHarness {
    pub fn new() -> Self {
        let inbox_root_dir = tempfile::Builder::new()
            .prefix("ox-crash-subprocess-inbox-")
            .tempdir()
            .expect("create inbox temp dir");
        let workspace_dir = tempfile::Builder::new()
            .prefix("ox-crash-subprocess-workspace-")
            .tempdir()
            .expect("create workspace temp dir");
        let inbox_root = inbox_root_dir.path().to_path_buf();
        let workspace = workspace_dir.path().to_path_buf();
        seed_subprocess_config(&inbox_root);
        Self {
            inbox_root,
            workspace,
            _inbox_root_dir: inbox_root_dir,
            _workspace_dir: workspace_dir,
        }
    }

    /// Spawn the real `ox` binary against this harness's temp dir.
    ///
    /// `env("HOME")` is set to `inbox_root.parent()` so the binary's
    /// `~/.ox` resolution lands in our sandbox. `OX_TEST_FREEZE_AT` and
    /// `OX_TEST_FAKE_TRANSPORT_SCRIPT` are reserved for future tasks.
    pub fn spawn(&self) -> std::process::Child {
        let bin = cargo_bin_path();
        let home = self
            .inbox_root
            .parent()
            .expect("inbox_root has a parent")
            .to_path_buf();
        let mut cmd = std::process::Command::new(&bin);
        cmd.env("HOME", &home)
            .env("OX_INBOX_ROOT_OVERRIDE", &self.inbox_root)
            .env("OX_LOG", "off")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd.spawn()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()))
    }
}

fn seed_subprocess_config(inbox_root: &Path) {
    // Minimal config so `ox` doesn't enter the setup wizard.
    std::fs::create_dir_all(inbox_root).expect("mk .ox");
    let config_toml = r#"
[gate.defaults]
model = "claude-sonnet-4-20250514"
account = "anthropic"
max_tokens = 4096

[gate.accounts.anthropic]
provider = "anthropic"
"#;
    std::fs::write(inbox_root.join("config.toml"), config_toml).expect("write config.toml");
    let keys_dir = inbox_root.join("keys");
    std::fs::create_dir_all(&keys_dir).expect("mk keys");
    std::fs::write(keys_dir.join("anthropic.key"), b"fake-subprocess-key\n").expect("write key");
}

/// Resolve the path to the compiled `ox` binary. Uses `CARGO_BIN_EXE_ox` when
/// present (cargo sets this for integration tests linked against a bin
/// target); falls back to the workspace `target/debug` layout.
pub fn cargo_bin_path() -> PathBuf {
    if let Some(p) = std::env::var_os("CARGO_BIN_EXE_ox") {
        return PathBuf::from(p);
    }
    // Fallback for ad-hoc runs (`cargo test --test foo` may not set it if the
    // test is declared without `required-features`; we keep both paths for
    // robustness).
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_target = manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|r| r.join("target"));
    let workspace_target =
        workspace_target.unwrap_or_else(|| manifest.join("..").join("..").join("target"));
    let ext = if cfg!(windows) { ".exe" } else { "" };
    workspace_target.join("debug").join(format!("ox{ext}"))
}

// ---------------------------------------------------------------------------
// LedgerWriter freeze-point hook (Step 5 — scaffolded, inert today).
//
// The environment-variable protocol is the test's contract with the
// `LedgerWriter` thread. When Task 1a introduces `LedgerWriter`, it honors
// these variables to park before `sync_data()` etc. Today the helpers exist
// so the crash scenarios can reference them without a second refactor pass.
// ---------------------------------------------------------------------------

/// Freeze points the `LedgerWriter` (Task 1a onwards) can stop at. Passed via
/// the `OX_TEST_FREEZE_AT` env var as the `as_str()` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezePoint {
    /// Before `write_all`.
    BeforeWrite,
    /// Between `write_all` and `sync_data`.
    AfterWriteBeforeSync,
    /// After `sync_data`, before the ack channel resolves.
    AfterSync,
}

impl FreezePoint {
    pub fn as_str(self) -> &'static str {
        match self {
            FreezePoint::BeforeWrite => "before_write",
            FreezePoint::AfterWriteBeforeSync => "after_write_before_sync",
            FreezePoint::AfterSync => "after_sync",
        }
    }
}

/// Name of the environment variable the `LedgerWriter` consults (Task 1a+).
pub const OX_TEST_FREEZE_AT: &str = "OX_TEST_FREEZE_AT";

/// Name of the environment variable controlling whether a subprocess uses a
/// fake transport script instead of the real reqwest client (future task).
pub const OX_TEST_FAKE_TRANSPORT_SCRIPT: &str = "OX_TEST_FAKE_TRANSPORT_SCRIPT";

/// Marker used to drop the current `SharedLog` to a sidecar file. Honored by
/// the subprocess on startup so tests can read the captured `Vec<LogEntry>`
/// after kill. Wired in a later task.
pub const OX_DUMP_SHARED_LOG_ON: &str = "OX_DUMP_SHARED_LOG_ON";

/// Small Arc handle that future components can hold if they need to observe
/// the active freeze point without an env-var read on a hot path.
#[derive(Clone, Default)]
pub struct FreezePointHandle(Arc<parking_lot_lite::Cell<Option<FreezePoint>>>);

impl FreezePointHandle {
    pub fn from_env() -> Self {
        let fp = std::env::var(OX_TEST_FREEZE_AT).ok().and_then(|s| {
            Some(match s.as_str() {
                "before_write" => FreezePoint::BeforeWrite,
                "after_write_before_sync" => FreezePoint::AfterWriteBeforeSync,
                "after_sync" => FreezePoint::AfterSync,
                _ => return None,
            })
        });
        let handle = Self::default();
        handle.0.set(fp);
        handle
    }

    pub fn get(&self) -> Option<FreezePoint> {
        self.0.get()
    }
}

/// Tiny dependency-free wrapper for an interior-mutable Cell<Option<Copy>>,
/// sharing across `Arc` without requiring `Mutex`. Confined to the test tree
/// so we don't add a runtime dependency.
mod parking_lot_lite {
    use std::sync::atomic::{AtomicU8, Ordering};

    #[derive(Default)]
    pub struct Cell<T: Copy + Default> {
        state: AtomicU8,
        value: std::sync::Mutex<T>,
    }

    impl<T: Copy + Default> Cell<T> {
        pub fn set(&self, v: T) {
            *self.value.lock().unwrap() = v;
            self.state.store(1, Ordering::Release);
        }
        pub fn get(&self) -> T {
            if self.state.load(Ordering::Acquire) == 0 {
                T::default()
            } else {
                *self.value.lock().unwrap()
            }
        }
    }
}
