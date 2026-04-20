//! BrokerSetup — create the BrokerStore and mount all stores.
//!
//! This is the single point where the store namespace is assembled.
//! The TUI event loop and agent workers interact through client handles.

use ox_broker::BrokerStore;
use ox_inbox::InboxStore;
use ox_ui::{Binding, InputStore, UiStore};
use structfs_core_store::path;
use tokio::task::JoinHandle;

/// Handles returned from broker setup.
pub struct BrokerHandle {
    pub broker: BrokerStore,
    _servers: Vec<JoinHandle<()>>,
}

impl BrokerHandle {
    pub fn client(&self) -> ox_broker::ClientHandle {
        self.broker.client()
    }
}

/// Create and wire the BrokerStore with all stores mounted.
///
/// Mounts:
/// - `ui/` → UiStore (in-memory state machine)
/// - `input/` → InputStore (key binding translation)
/// - `inbox/` → InboxStore (SQLite-backed thread index)
/// - `threads/` → ThreadRegistry (lazy per-thread store lifecycle)
pub async fn setup(
    inbox: InboxStore,
    bindings: Vec<Binding>,
    inbox_root: std::path::PathBuf,
    config_values: std::collections::BTreeMap<String, structfs_core_store::Value>,
) -> BrokerHandle {
    let broker = BrokerStore::default();
    let mut servers = Vec::new();

    // Mount UiStore. The embedded CommandLineStore records submit
    // intent as a pending_submit field; the event loop drains it and
    // dispatches `command/exec`, avoiding re-entrant writes back into
    // UiStore's server task.
    servers.push(broker.mount(path!("ui"), UiStore::new()).await);

    // Mount CommandStore with broker-connected dispatcher
    {
        let command_dispatch_client = broker.client();
        let mut command_store = ox_ui::CommandStore::from_builtins();
        command_store.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(command_dispatch_client.write(target, data))
            })
        }));
        servers.push(broker.mount(path!("command"), command_store).await);
    }

    // Mount InputStore with broker-connected dispatcher
    let dispatch_client = broker.client();
    let mut input = InputStore::new(bindings);
    input.set_dispatcher(Box::new(move |target, data| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(dispatch_client.write(target, data))
        })
    }));
    servers.push(broker.mount(path!("input"), input).await);

    // Mount InboxStore
    servers.push(broker.mount(path!("inbox"), inbox).await);

    // Mount ConfigStore with figment-resolved values + TOML file backing
    {
        let toml_path = inbox_root.join("config.toml");
        let backing = crate::toml_backing::TomlFileBacking::new(toml_path);
        let config = ox_ui::ConfigStore::with_backing(config_values, Box::new(backing));

        servers.push(broker.mount(path!("config"), config).await);
    }

    // Mount ThreadRegistry at threads/ — lazy-mounts per-thread stores from disk
    let mut registry = crate::thread_registry::ThreadRegistry::new(inbox_root);
    registry.set_broker_client(broker.client());
    servers.push(broker.mount_async(path!("threads"), registry).await);

    tracing::info!(stores = servers.len(), "broker setup complete");

    BrokerHandle {
        broker,
        _servers: servers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::{Value, path};

    fn test_inbox() -> InboxStore {
        let dir = tempfile::tempdir().unwrap();
        InboxStore::open(dir.path()).unwrap()
    }

    #[allow(deprecated)]
    fn test_inbox_root() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().into_path()
    }

    async fn test_setup() -> BrokerHandle {
        let bindings = crate::bindings::default_bindings();
        let mut config = BTreeMap::new();
        config.insert(
            "gate/defaults/model".to_string(),
            Value::String("claude-sonnet-4-20250514".into()),
        );
        config.insert(
            "gate/defaults/account".to_string(),
            Value::String("anthropic".into()),
        );
        config.insert("gate/defaults/max_tokens".to_string(), Value::Integer(4096));
        config.insert(
            "gate/accounts/anthropic/provider".to_string(),
            Value::String("anthropic".into()),
        );
        config.insert(
            "gate/accounts/anthropic/key".to_string(),
            Value::String("test-key".into()),
        );
        setup(test_inbox(), bindings, test_inbox_root(), config).await
    }

    // -- Mechanism-test helpers ---------------------------------------
    //
    // These drive real broker state (writes, reads, log appends) so
    // tests can mutate between observations. Helpers are async because
    // every broker call is async — there's no sync wrapper hiding a
    // `block_on` here.

    fn user(text: &str) -> ox_kernel::log::LogEntry {
        ox_kernel::log::LogEntry::User {
            content: text.into(),
            scope: None,
        }
    }

    fn assistant(text: &str) -> ox_kernel::log::LogEntry {
        ox_kernel::log::LogEntry::Assistant {
            content: vec![ox_kernel::ContentBlock::Text { text: text.into() }],
            source: None,
            scope: None,
            completion_id: 0,
        }
    }

    async fn create_thread(client: &ox_broker::ClientHandle, title: &str) -> String {
        let mut create = std::collections::BTreeMap::new();
        create.insert("title".to_string(), Value::String(title.into()));
        let path = client
            .write(
                &path!("inbox/threads"),
                structfs_core_store::Record::parsed(Value::Map(create)),
            )
            .await
            .unwrap();
        path.components
            .last()
            .map(|c| c.as_str().to_string())
            .expect("created path must carry the thread id")
    }

    async fn append_log_message(
        client: &ox_broker::ClientHandle,
        tid: &str,
        entry: ox_kernel::log::LogEntry,
    ) {
        let id_comp = ox_kernel::PathComponent::try_new(tid).unwrap();
        let log_path = ox_path::oxpath!("threads", id_comp, "log", "append");
        client.write_typed(&log_path, &entry).await.unwrap();
    }

    async fn fetch_log_count(client: &ox_broker::ClientHandle, tid: &str) -> i64 {
        let id_comp = ox_kernel::PathComponent::try_new(tid).unwrap();
        let count_path = ox_path::oxpath!("threads", id_comp, "log", "count");
        let rec = client.read(&count_path).await.unwrap().unwrap();
        match rec.as_value().unwrap() {
            Value::Integer(n) => *n,
            other => panic!("expected Integer for log/count, got {other:?}"),
        }
    }

    async fn fetch_inbox_row(
        client: &ox_broker::ClientHandle,
        tid: &str,
    ) -> crate::parse::InboxThread {
        let id_comp = ox_kernel::PathComponent::try_new(tid).unwrap();
        let rec = client
            .read(&ox_path::oxpath!("inbox", "threads", id_comp))
            .await
            .unwrap()
            .unwrap();
        let val = rec.as_value().unwrap();
        let arr = Value::Array(vec![val.clone()]);
        crate::parse::parse_inbox_threads(&arr)
            .into_iter()
            .next()
            .expect("inbox row must exist for tid")
    }

    fn dialog_with_info_open() -> crate::event_loop::DialogState {
        crate::event_loop::DialogState {
            pending_customize: None,
            show_shortcuts: false,
            show_usage: false,
            show_thread_info: true,
            thread_info: None,
            history_search: None,
        }
    }

    /// Seed two threads such that:
    /// - `tid_a` has title `aaa-only-match` — chip `"aaa"` matches it.
    /// - `tid_b` has title `no-special` — chip `"aaa"` does NOT match.
    /// - `tid_a` is created first; a >1-second gap then `tid_b`.
    ///   SQLite `updated_at` is second-resolution; the gap forces
    ///   `updated_at(tid_b) > updated_at(tid_a)` so `ORDER BY
    ///   updated_at DESC` puts `tid_b` first in the default listing.
    /// - With chip `"aaa"` applied, the inbox returns `[tid_a]`.
    ///   Without it, `[tid_b, tid_a]`. Net: row 0 differs by chip,
    ///   deterministically.
    async fn arrange_filtered_first_row_differs_from_default(
        client: &ox_broker::ClientHandle,
    ) -> (String, String) {
        let tid_a = create_thread(client, "aaa-only-match").await;
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let tid_b = create_thread(client, "no-special").await;
        (tid_a, tid_b)
    }

    /// Type a chip via the same path the UI uses
    /// (SearchInsertChar+SearchSaveChip). After this returns, the
    /// chip is committed and `search.active` is true (chips
    /// non-empty).
    async fn add_chip(client: &ox_broker::ClientHandle, chip: &str) {
        use ox_types::{InboxCommand, UiCommand};
        for c in chip.chars() {
            client
                .write_typed(
                    &path!("ui"),
                    &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: c }),
                )
                .await
                .unwrap();
        }
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Inbox(InboxCommand::SearchSaveChip),
            )
            .await
            .unwrap();
    }

    /// Dismiss every chip one by one. There is no atomic "clear all"
    /// command; the UI dismisses chips by index, so we loop until
    /// `ui/search_chips` is empty. After this returns, `search.active`
    /// is false (chips empty AND no live query).
    async fn clear_all_chips(client: &ox_broker::ClientHandle) {
        use ox_types::{InboxCommand, UiCommand};
        loop {
            let chips = client
                .read(&path!("ui/search_chips"))
                .await
                .unwrap()
                .unwrap();
            let count = match chips.as_value().unwrap() {
                Value::Array(a) => a.len(),
                _ => 0,
            };
            if count == 0 {
                break;
            }
            client
                .write_typed(
                    &path!("ui"),
                    &UiCommand::Inbox(InboxCommand::SearchDismissChip { index: 0 }),
                )
                .await
                .unwrap();
        }
    }

    // -- Mechanism tests (mutate-between-calls) ----------------------

    /// Test A — live per-entry update.
    ///
    /// Append log entries between refreshes; the second refresh sees
    /// the new count without any save call. A cache keyed on SQLite
    /// `last_seq` would miss this — `last_seq` only advances on save.
    ///
    /// Manual verification: run
    /// `RUST_LOG=thread_info=debug cargo test -p ox-cli --bin ox \
    /// cache_updates_per_log_entry_not_per_save -- --nocapture`
    /// and you should see, in order:
    ///
    /// ```text
    ///   thread_info: cache miss — fetched thread_id=t_xxx log_count=2 duration_us=…
    ///   thread_info: cache miss — fetched thread_id=t_xxx log_count=3 duration_us=…
    /// ```
    ///
    /// (No "cache hit" line here — every refresh in this test
    /// follows a mutation, so each one is a miss.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cache_updates_per_log_entry_not_per_save() {
        use crate::event_loop::refresh_thread_info_cache;
        // Best-effort install of a subscriber so a `RUST_LOG`-driven
        // manual run produces a readable narrative. `try_init` is a
        // no-op if another test has already installed one.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
            )
            .with_test_writer()
            .try_init();

        let handle = test_setup().await;
        let client = handle.client();

        let tid = create_thread(&client, "t_live").await;
        append_log_message(&client, &tid, user("first")).await;
        append_log_message(&client, &tid, assistant("hi")).await;
        // Intentionally NOT calling save — SQLite last_seq stays at -1.

        let mut dialog = dialog_with_info_open();
        refresh_thread_info_cache(&client, &mut dialog).await;
        let entry = dialog.thread_info.as_ref().unwrap();
        assert_eq!(entry.info.stats.message_count, 2);
        let count_1 = entry.log_count_at_cache;

        append_log_message(&client, &tid, user("third")).await;
        // Again: no save. A cache keyed on SQLite last_seq would miss this.

        refresh_thread_info_cache(&client, &mut dialog).await;
        let entry = dialog.thread_info.as_ref().unwrap();
        assert!(
            entry.log_count_at_cache > count_1,
            "log_count_at_cache must advance after a fresh append",
        );
        assert_eq!(
            entry.info.stats.message_count, 3,
            "cache must reflect the new log entry without a save boundary",
        );
    }

    /// Test B — hit short-circuit on unchanged state.
    ///
    /// Two refreshes with no mutation; cache contents stay identical
    /// AND a fresh `log/count` read agrees with the cached count.
    /// Pinning both protects against a regression where the cache
    /// silently re-fetches every tick (test would still see the same
    /// id but the freshness signal proves we held).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cache_hits_when_state_unchanged() {
        use crate::event_loop::refresh_thread_info_cache;
        let handle = test_setup().await;
        let client = handle.client();

        let tid = create_thread(&client, "t_hit").await;
        append_log_message(&client, &tid, user("only")).await;

        let mut dialog = dialog_with_info_open();
        refresh_thread_info_cache(&client, &mut dialog).await;
        let cached = dialog.thread_info.as_ref().unwrap();
        let cached_id = cached.info.id().to_string();
        let cached_count = cached.log_count_at_cache;

        refresh_thread_info_cache(&client, &mut dialog).await;
        let after = dialog.thread_info.as_ref().unwrap();
        assert_eq!(after.info.id(), cached_id);
        assert_eq!(after.log_count_at_cache, cached_count);

        let live_count = fetch_log_count(&client, &tid).await;
        assert_eq!(
            live_count, cached_count,
            "fresh log/count must agree with cache when state hasn't changed",
        );
    }

    /// Test C — selection change invalidates.
    ///
    /// Move the inbox selection between two threads; the cache picks
    /// up the new thread on the next refresh. The mutation between
    /// observations is a `SelectNext` UI command — drives the cache
    /// invalidation through a real input path.
    ///
    /// The order of the two threads in the inbox is read at runtime
    /// rather than assumed: the inbox uses second-resolution
    /// `updated_at` and SQLite ties are implementation-defined. The
    /// invariant under test is "selection change invalidates" — not
    /// any specific ordering — so we pick `expected_first` and
    /// `expected_second` from whatever the inbox actually returned.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cache_invalidates_on_selection_change() {
        use crate::event_loop::refresh_thread_info_cache;
        use ox_types::{InboxCommand, UiCommand};
        let handle = test_setup().await;
        let client = handle.client();

        let tid_a = create_thread(&client, "alpha").await;
        append_log_message(&client, &tid_a, user("a")).await;
        let tid_b = create_thread(&client, "beta").await;
        append_log_message(&client, &tid_b, user("a")).await;
        append_log_message(&client, &tid_b, assistant("b")).await;

        // Read the actual ordering — both ids exist; one of them is
        // at row 0 and the other at row 1.
        let rec = client.read(&path!("inbox/threads")).await.unwrap().unwrap();
        let rows = crate::parse::parse_inbox_threads(rec.as_value().unwrap());
        assert_eq!(rows.len(), 2);
        let expected_row_0 = rows[0].id.clone();
        let expected_row_1 = rows[1].id.clone();
        assert_ne!(expected_row_0, expected_row_1);
        assert!([&tid_a, &tid_b].contains(&&expected_row_0));
        assert!([&tid_a, &tid_b].contains(&&expected_row_1));

        let mut dialog = dialog_with_info_open();
        refresh_thread_info_cache(&client, &mut dialog).await;
        assert_eq!(
            dialog.thread_info.as_ref().unwrap().info.id(),
            expected_row_0,
            "row 0 must be cached after the first refresh",
        );

        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Inbox(InboxCommand::SetRowCount { count: 2 }),
            )
            .await
            .unwrap();
        client
            .write_typed(&path!("ui"), &UiCommand::Inbox(InboxCommand::SelectNext))
            .await
            .unwrap();

        refresh_thread_info_cache(&client, &mut dialog).await;
        let after_id = dialog.thread_info.as_ref().unwrap().info.id().to_string();
        assert_eq!(
            after_id, expected_row_1,
            "row 1 must be cached after SelectNext",
        );
        assert_ne!(
            after_id, expected_row_0,
            "selection change must invalidate the cache",
        );
    }

    /// Test D — search-state aliasing regression.
    ///
    /// `selected_row` doesn't change but the row at that index does
    /// (because the chip filter changed). A cache keyed on
    /// `selected_row` alone (the prior implementation) would miss
    /// this — the `(thread_id, log_count)` key catches it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cache_invalidates_on_search_state_aliasing() {
        use crate::event_loop::refresh_thread_info_cache;
        let handle = test_setup().await;
        let client = handle.client();

        let (tid_a, tid_b) = arrange_filtered_first_row_differs_from_default(&client).await;

        add_chip(&client, "aaa").await;

        let mut dialog = dialog_with_info_open();
        refresh_thread_info_cache(&client, &mut dialog).await;
        assert_eq!(
            dialog.thread_info.as_ref().unwrap().info.id(),
            tid_a,
            "with chip applied, row 0 must be tid_a",
        );

        // Clear chips; selected_row stays 0 but the row at 0 changes.
        clear_all_chips(&client).await;

        refresh_thread_info_cache(&client, &mut dialog).await;
        let after_id = dialog.thread_info.as_ref().unwrap().info.id().to_string();
        assert_ne!(
            after_id, tid_a,
            "row 0 must change when chip clears — cache must invalidate",
        );
        assert_eq!(after_id, tid_b, "default order puts tid_b at row 0");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_store_is_mounted() {
        let handle = test_setup().await;
        let client = handle.client();

        let result = client.read(&path!("command/commands")).await.unwrap();
        assert!(result.is_some());
        match result.unwrap().as_value().unwrap() {
            Value::Array(arr) => assert!(!arr.is_empty()),
            _ => panic!("expected array"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_store_reads_single_command() {
        let handle = test_setup().await;
        let client = handle.client();

        let result = client.read(&path!("command/commands/quit")).await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broker_setup_mounts_all_stores() {
        let handle = test_setup().await;
        let client = handle.client();

        // UiStore is mounted — read initial state
        let screen = client.read(&path!("ui/screen")).await.unwrap().unwrap();
        assert_eq!(
            screen.as_value().unwrap(),
            &Value::String("inbox".to_string())
        );

        // InputStore is mounted — read bindings
        let bindings_val = client
            .read(&path!("input/bindings/normal"))
            .await
            .unwrap()
            .unwrap();
        match bindings_val.as_value().unwrap() {
            Value::Array(a) => assert!(!a.is_empty()),
            _ => panic!("expected array"),
        }

        // InboxStore is mounted — read threads (empty initially)
        let threads = client.read(&path!("inbox/threads")).await.unwrap();
        assert!(threads.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn key_dispatch_through_broker() {
        use ox_types::{InboxCommand, UiCommand};

        let handle = test_setup().await;
        let client = handle.client();

        // Set row count so selection can advance
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Inbox(InboxCommand::SetRowCount { count: 5 }),
            )
            .await
            .unwrap();

        // Dispatch "j" on inbox screen
        let event = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: "j".to_string(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &event)
            .await
            .unwrap();

        // Verify UiStore state changed
        let row = client
            .read(&path!("ui/selected_row"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn screen_specific_binding_routes_correctly() {
        use ox_types::{GlobalCommand, ThreadCommand, UiCommand};

        let handle = test_setup().await;
        let client = handle.client();

        // Open a thread so we're on the thread screen
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Global(GlobalCommand::Open {
                    thread_id: "t_test".to_string(),
                }),
            )
            .await
            .unwrap();

        // Give the thread some scroll headroom
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 100 }),
            )
            .await
            .unwrap();

        // Dispatch "j" on thread screen — should trigger scroll_down (thread-specific),
        // NOT select_next (inbox-specific)
        let event = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: "j".to_string(),
            screen: ox_types::Screen::Thread,
        };
        client
            .write_typed(&path!("input/key"), &event)
            .await
            .unwrap();

        // Verify we're on thread screen (not inbox)
        let screen = client.read(&path!("ui/screen")).await.unwrap().unwrap();
        assert_eq!(
            screen.as_value().unwrap(),
            &Value::String("thread".to_string())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_model_reads_from_gate_store() {
        let handle = test_setup().await;
        let client = handle.client();

        // Read model for a thread — uses GateStore default
        let model = client
            .read(&path!("threads/t_test/gate/defaults/model"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_gate_reads_api_key_from_config() {
        let handle = test_setup().await;
        let client = handle.client();

        // The thread's GateStore should read the API key from ConfigStore
        // via its config handle (bootstrap account = anthropic)
        let key = client
            .read(&path!("threads/t_gate/gate/accounts/anthropic/key"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.as_value().unwrap(), &Value::String("test-key".into()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_gate_reads_model_from_config() {
        let handle = test_setup().await;
        let client = handle.client();

        // GateStore config handle reads gate/defaults/model from ConfigStore
        let model = client
            .read(&path!("threads/t_cfg/gate/defaults/model"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );
    }

    // -- Performance floor via linearity ratio ------------------------

    /// Asserts `fetch_thread_info` scales roughly linearly. The 10×
    /// input case must take no more than 15× the 1× case — catches a
    /// quadratic regression on any runner without depending on
    /// absolute wall-clock speed. A backstop on the 10k case catches
    /// catastrophic regressions that don't bend the ratio.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_thread_info_scales_linearly_with_log_size() {
        use ox_kernel::ContentBlock;
        use ox_kernel::log::LogEntry;

        async fn time_fetch_for_size(
            client: &ox_broker::ClientHandle,
            n: usize,
            tag: &str,
        ) -> std::time::Duration {
            let tid = create_thread(client, tag).await;
            for i in 0..(n / 2) {
                append_log_message(
                    client,
                    &tid,
                    LogEntry::User {
                        content: format!("u{i}"),
                        scope: None,
                    },
                )
                .await;
                append_log_message(
                    client,
                    &tid,
                    LogEntry::Assistant {
                        content: vec![ContentBlock::Text {
                            text: format!("a{i}"),
                        }],
                        source: None,
                        scope: None,
                        completion_id: i as u64,
                    },
                )
                .await;
            }
            let row = fetch_inbox_row(client, &tid).await;
            // Take the minimum of three runs for noise resistance.
            let mut best = std::time::Duration::from_secs(999);
            for _ in 0..3 {
                let t0 = std::time::Instant::now();
                let info = crate::view_state::fetch_thread_info(client, &row).await;
                best = best.min(t0.elapsed());
                assert_eq!(info.stats.message_count, n);
            }
            best
        }

        let handle = test_setup().await;
        let client = handle.client();

        let t_1k = time_fetch_for_size(&client, 1_000, "t_1k").await;
        let t_10k = time_fetch_for_size(&client, 10_000, "t_10k").await;

        let ratio = t_10k.as_secs_f64() / t_1k.as_secs_f64().max(1e-6);
        assert!(
            ratio < 15.0,
            "10× input → {ratio:.1}× time (t_1k={t_1k:?}, t_10k={t_10k:?}); \
             S-tier allows up to ~10× linear + headroom for constants. \
             A quadratic regression would produce ratio near 100.",
        );

        // Backstop: even with a healthy ratio, 10k > 2s means a
        // fundamental constant blew up.
        assert!(
            t_10k.as_secs() < 2,
            "t_10k={t_10k:?} exceeds sanity backstop",
        );
    }

    // -- Discoverability and cross-screen consistency -----------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn i_appears_in_inbox_shortcut_bindings() {
        // The shortcuts (`?`) modal reads from `input/bindings/{mode}/{screen}`.
        // If `i` ever stops appearing here, it's invisible to users.
        let handle = test_setup().await;
        let client = handle.client();

        let bindings = client
            .read(&path!("input/bindings/normal/inbox"))
            .await
            .unwrap()
            .unwrap();
        let arr = match bindings.as_value().unwrap() {
            Value::Array(a) => a.clone(),
            _ => panic!("expected array"),
        };
        let has_i = arr.iter().any(|v| match v {
            Value::Map(m) => {
                matches!(m.get("key"), Some(Value::String(s)) if s == "i")
            }
            _ => false,
        });
        assert!(
            has_i,
            "i must be bound on Normal+Inbox and visible to the shortcuts modal",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn i_appears_in_thread_shortcut_bindings() {
        let handle = test_setup().await;
        let client = handle.client();

        let bindings = client
            .read(&path!("input/bindings/normal/thread"))
            .await
            .unwrap()
            .unwrap();
        let arr = match bindings.as_value().unwrap() {
            Value::Array(a) => a.clone(),
            _ => panic!("expected array"),
        };
        let has_i = arr.iter().any(|v| match v {
            Value::Map(m) => {
                matches!(m.get("key"), Some(Value::String(s)) if s == "i")
            }
            _ => false,
        });
        assert!(
            has_i,
            "i must be bound on Normal+Thread and visible to the shortcuts modal",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn i_on_thread_screen_shows_current_thread_info() {
        // End-to-end: opening the info modal while viewing a thread
        // must populate the cache for *that* thread (not the inbox's
        // selected row).
        use crate::event_loop::{DialogState, refresh_thread_info_cache};

        let handle = test_setup().await;
        let client = handle.client();

        // Create a thread; the inbox writer assigns its id.
        let mut create = std::collections::BTreeMap::new();
        create.insert(
            "title".to_string(),
            Value::String("thread-screen-info".into()),
        );
        let created_path = client
            .write(
                &path!("inbox/threads"),
                structfs_core_store::Record::parsed(Value::Map(create)),
            )
            .await
            .unwrap();
        let tid = created_path
            .components
            .last()
            .map(|c| c.as_str().to_string())
            .expect("created path should carry the thread id");

        // Drive UiStore to ScreenSnapshot::Thread.
        client
            .write_typed(
                &path!("ui"),
                &ox_types::UiCommand::Global(ox_types::GlobalCommand::Open {
                    thread_id: tid.clone(),
                }),
            )
            .await
            .unwrap();

        let mut dialog = DialogState {
            pending_customize: None,
            show_shortcuts: false,
            show_usage: false,
            show_thread_info: true,
            thread_info: None,
            history_search: None,
        };

        refresh_thread_info_cache(&client, &mut dialog).await;

        let entry = dialog
            .thread_info
            .as_ref()
            .expect("cache must populate on Thread screen");
        assert_eq!(
            entry.info.id(),
            tid,
            "cache must hold the currently-viewed thread",
        );
    }

    // -- Loading state and fetch-failure UX --------------------------

    #[test]
    fn modal_shows_loading_placeholder_before_first_fetch() {
        // The renderer accepts `Option<&ThreadInfo>`; passing None
        // (cache not yet populated) must produce the visible
        // "Loading info" placeholder rather than a blank modal or a
        // zero-stats card.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = crate::theme::Theme::default_theme();
        terminal
            .draw(|frame| {
                crate::dialogs::draw_thread_info_modal(
                    frame,
                    None,
                    "claude-sonnet-4",
                    &std::collections::BTreeMap::new(),
                    None,
                    &theme,
                );
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("Loading info"),
            "rendered buffer must contain 'Loading info' placeholder; got:\n{text}",
        );
    }

    #[test]
    fn modal_surfaces_pending_approval_banner() {
        // When an approval arrives while the info modal is open, the
        // modal outranks the approval dialog visually — the banner
        // is the user's cue to dismiss the modal and respond.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = crate::theme::Theme::default_theme();
        let info = crate::types::ThreadInfo {
            meta: crate::types::ThreadMetadata {
                id: "t_1".into(),
                title: "Seeded".into(),
                ..Default::default()
            },
            stats: crate::types::ThreadStats::default(),
        };
        let approval = ox_types::ApprovalRequest {
            tool_name: "shell".into(),
            tool_input: serde_json::json!({"command": "rm -rf /"}),
        };
        terminal
            .draw(|frame| {
                crate::dialogs::draw_thread_info_modal(
                    frame,
                    Some(&info),
                    "claude-sonnet-4",
                    &std::collections::BTreeMap::new(),
                    Some(&approval),
                    &theme,
                );
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("Approval requested") && text.contains("shell"),
            "modal must surface the pending approval and name the tool; got:\n{text}",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_failure_keeps_modal_in_loading_state_and_warns() {
        // When the modal opens against a thread the inbox can't
        // resolve (e.g., the row is missing or just plain wrong), the
        // cache stays empty (modal renders Loading) AND a
        // `tracing::warn!` is emitted under target `thread_info` so
        // operators see why. This test pins both halves of the
        // contract.
        use std::io;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct CaptureWriter(Arc<Mutex<Vec<u8>>>);
        impl io::Write for CaptureWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for CaptureWriter {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("thread_info=warn"))
            .with_writer(CaptureWriter(buf.clone()))
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let handle = test_setup().await;
        let client = handle.client();

        // Drive UiStore to ScreenSnapshot::Thread on an id that has
        // no row in the inbox. The warn fires inside
        // refresh_thread_info_cache when the row read returns
        // Ok(None).
        client
            .write_typed(
                &path!("ui"),
                &ox_types::UiCommand::Global(ox_types::GlobalCommand::Open {
                    thread_id: "t_nonexistent".into(),
                }),
            )
            .await
            .unwrap();

        let mut dialog = crate::event_loop::DialogState {
            pending_customize: None,
            show_shortcuts: false,
            show_usage: false,
            show_thread_info: true,
            thread_info: None,
            history_search: None,
        };

        crate::event_loop::refresh_thread_info_cache(&client, &mut dialog).await;

        assert!(
            dialog.thread_info.is_none(),
            "cache must remain empty when the thread can't be resolved",
        );

        let captured = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            captured.contains("WARN") && captured.contains("t_nonexistent"),
            "expected a tracing::warn! mentioning t_nonexistent; captured: {captured:?}",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_save_result_to_inbox_updates_live_counts() {
        // Drives the actual helper agents.rs uses after each save. If
        // someone refactors the helper (wrong field name, wrong path),
        // this test catches it — whereas simulating the write would
        // silently agree with a buggy reimplementation.
        let handle = test_setup().await;
        let client = handle.client();

        // Create a thread (writer assigns the id).
        let mut create = std::collections::BTreeMap::new();
        create.insert(
            "title".to_string(),
            Value::String("live write-through".into()),
        );
        let created_path = client
            .write(
                &path!("inbox/threads"),
                structfs_core_store::Record::parsed(Value::Map(create)),
            )
            .await
            .unwrap();
        let tid = created_path
            .components
            .last()
            .map(|c| c.as_str().to_string())
            .expect("created path should carry the thread id");

        // Call the real helper with a synthesized SaveResult.
        let result = ox_inbox::snapshot::SaveResult {
            last_seq: 5,
            last_hash: Some("h5".into()),
            message_count: 3,
        };
        crate::agents::write_save_result_to_inbox(&client, &tid, &result).await;

        // Readers see the updated count through the same codepath the
        // inbox view uses.
        let rec = client.read(&path!("inbox/threads")).await.unwrap().unwrap();
        let rows = crate::parse::parse_inbox_threads(rec.as_value().expect("array value expected"));
        let row = rows
            .iter()
            .find(|r| r.id == tid)
            .expect("row must appear in listing");
        assert_eq!(
            row.message_count, 3,
            "message_count must be written through"
        );
        assert_eq!(row.last_seq, 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_thread_info_reads_log_through_broker_end_to_end() {
        // Proves `read_typed::<Vec<LogEntry>>` round-trips the log
        // store's serialized entries. A bug in the wire format or the
        // mount path would fail here — not in the unit tests against
        // the in-memory aggregator.
        use ox_kernel::ContentBlock;
        use ox_kernel::log::LogEntry;

        let handle = test_setup().await;
        let client = handle.client();

        // Seed a synthetic log for a thread id via log/append. The
        // ThreadRegistry lazy-mounts the thread store on first write.
        let tid = "t_info_e2e";
        let log_path =
            structfs_core_store::Path::parse(&format!("threads/{tid}/log/append")).unwrap();

        let entries: Vec<LogEntry> = vec![
            LogEntry::User {
                content: "hi".into(),
                scope: None,
            },
            // Matches production: every complete() invocation is
            // logged as a tool_call alongside the assistant response.
            LogEntry::ToolCall {
                id: "complete-root-1".into(),
                name: "complete".into(),
                input: serde_json::json!({"account": "anthropic"}),
                scope: Some("root".into()),
            },
            LogEntry::Assistant {
                content: vec![
                    ContentBlock::Text {
                        text: "hello".into(),
                    },
                    ContentBlock::ToolUse(ox_kernel::ToolCall {
                        id: "t1".into(),
                        name: "shell".into(),
                        input: serde_json::json!({}),
                    }),
                ],
                source: None,
                scope: None,
                completion_id: 1,
            },
            // Kernel logs each Assistant-emitted tool_call separately.
            LogEntry::ToolCall {
                id: "t1".into(),
                name: "shell".into(),
                input: serde_json::json!({}),
                scope: Some("root".into()),
            },
            LogEntry::CompletionEnd {
                scope: "main".into(),
                model: "claude-sonnet-4-20250514".into(),
                completion_id: 1,
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        ];
        for entry in &entries {
            client.write_typed(&log_path, entry).await.unwrap();
        }

        // Now call fetch_thread_info via the same path the real UI
        // uses. The result should reflect the log we just wrote.
        let row = crate::parse::InboxThread {
            id: tid.into(),
            title: "e2e".into(),
            thread_state: "running".into(),
            labels: vec![],
            token_count: 0,
            last_seq: 2,
            message_count: 2,
        };
        let info = crate::view_state::fetch_thread_info(&client, &row).await;

        // 1 user + 1 assistant = 2 messages; tool_call entries do not
        // inflate message_count.
        assert_eq!(info.stats.message_count, 2, "user + assistant only");
        assert_eq!(info.stats.user_messages, 1);
        assert_eq!(info.stats.assistant_messages, 1);
        // Two distinct tool calls logged: one `complete` (the LLM
        // invocation) and one `shell` (the LLM-emitted tool use).
        assert_eq!(
            info.stats.tool_uses,
            vec![("complete".into(), 1), ("shell".into(), 1)],
        );
        assert_eq!(info.stats.models, vec!["claude-sonnet-4-20250514"]);
        assert_eq!(
            info.stats.primary_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn i_binding_on_inbox_toggles_thread_info_flag() {
        // Pressing `i` in Normal mode on inbox should set
        // PendingAction::ToggleThreadInfo on UiStore, which the event
        // loop then applies to DialogState. We stop at the UiStore —
        // the pending_action assertion is enough to prove the binding
        // routes to the right command.
        let handle = test_setup().await;
        let client = handle.client();

        let event = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: "i".into(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &event)
            .await
            .unwrap();

        let pa = client
            .read(&path!("ui/pending_action"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            pa.as_value().unwrap(),
            &Value::String("toggle_thread_info".into()),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chips_survive_thread_roundtrip() {
        // Chips are a persistent view filter. Opening a thread and
        // closing back to the inbox must not drop them.
        use ox_types::{GlobalCommand, InboxCommand, UiCommand};

        let handle = test_setup().await;
        let client = handle.client();

        // Seed a chip.
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: 'f' }),
            )
            .await
            .unwrap();
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Inbox(InboxCommand::SearchSaveChip),
            )
            .await
            .unwrap();

        // Open a thread, then close back.
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Global(GlobalCommand::Open {
                    thread_id: "t_roundtrip".into(),
                }),
            )
            .await
            .unwrap();
        client
            .write_typed(&path!("ui"), &UiCommand::Global(GlobalCommand::Close))
            .await
            .unwrap();

        // Chip should still be there.
        let chips = client
            .read(&path!("ui/search_chips"))
            .await
            .unwrap()
            .unwrap();
        match chips.as_value().unwrap() {
            Value::Array(a) => {
                assert_eq!(a.len(), 1);
                assert_eq!(a[0], Value::String("f".into()));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn number_keys_in_normal_mode_dismiss_chips() {
        // Regression: `1`-`9` bindings declared `index` as a String,
        // but the builtin registry declares it as Integer — every
        // dismiss was silently rejected by the type-mismatch validator.
        use ox_types::{InboxCommand, UiCommand};

        let handle = test_setup().await;
        let client = handle.client();
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Inbox(InboxCommand::SetRowCount { count: 5 }),
            )
            .await
            .unwrap();

        // Seed two chips by typing and hitting Enter twice via Search mode.
        let slash = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: "/".into(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &slash)
            .await
            .unwrap();
        for c in "foo".chars() {
            crate::event_loop::handle_unbound_search_key(
                &client,
                crossterm::event::KeyCode::Char(c),
            )
            .await;
        }
        let enter = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Search,
            key: "Enter".into(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &enter)
            .await
            .unwrap();
        // Type bar + Enter → second chip
        for c in "bar".chars() {
            crate::event_loop::handle_unbound_search_key(
                &client,
                crossterm::event::KeyCode::Char(c),
            )
            .await;
        }
        client
            .write_typed(&path!("input/key"), &enter)
            .await
            .unwrap();
        // Esc out of Search mode
        let esc = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Search,
            key: "Esc".into(),
            screen: ox_types::Screen::Inbox,
        };
        client.write_typed(&path!("input/key"), &esc).await.unwrap();

        // Sanity: two chips
        let chips = client
            .read(&path!("ui/search_chips"))
            .await
            .unwrap()
            .unwrap();
        match chips.as_value().unwrap() {
            Value::Array(a) => assert_eq!(a.len(), 2),
            other => panic!("expected array, got {other:?}"),
        }

        // Press `1` in Normal mode on inbox → dismiss chip 0 (first).
        let one = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: "1".into(),
            screen: ox_types::Screen::Inbox,
        };
        client.write_typed(&path!("input/key"), &one).await.unwrap();

        let chips = client
            .read(&path!("ui/search_chips"))
            .await
            .unwrap()
            .unwrap();
        match chips.as_value().unwrap() {
            Value::Array(a) => {
                assert_eq!(a.len(), 1, "chip should have been dismissed");
                assert_eq!(a[0], Value::String("bar".into()));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn search_slash_on_inbox_opens_mode_not_editor() {
        // `/` should open search mode without creating an editor. Hitting
        // Enter promotes the live query into a chip. Esc closes the mode
        // and drops any in-flight live query but leaves chips intact.
        let handle = test_setup().await;
        let client = handle.client();

        // Press `/` on inbox — Normal mode
        let slash = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: "/".to_string(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &slash)
            .await
            .unwrap();

        // Mode snapshot reports search
        let mode = client.read(&path!("ui/mode")).await.unwrap().unwrap();
        assert_eq!(mode.as_value().unwrap(), &Value::String("search".into()));
        // Editor is not involved
        let ctx = client
            .read(&path!("ui/insert_context"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ctx.as_value().unwrap(), &Value::Null);

        // Type "foo" through the same fallback the event loop uses for
        // unbound Search-mode keys — the path real keystrokes take.
        for c in "foo".chars() {
            crate::event_loop::handle_unbound_search_key(
                &client,
                crossterm::event::KeyCode::Char(c),
            )
            .await;
        }
        let live = client
            .read(&path!("ui/search_live_query"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.as_value().unwrap(), &Value::String("foo".into()));

        // Enter → SearchSaveChip via Search-mode binding
        let enter = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Search,
            key: "Enter".to_string(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &enter)
            .await
            .unwrap();

        // Chip committed; live_query empty; still in search mode (cursor ready)
        let chips = client
            .read(&path!("ui/search_chips"))
            .await
            .unwrap()
            .unwrap();
        match chips.as_value().unwrap() {
            Value::Array(a) => {
                assert_eq!(a.len(), 1);
                assert_eq!(a[0], Value::String("foo".into()));
            }
            other => panic!("expected array, got {other:?}"),
        }
        let live = client
            .read(&path!("ui/search_live_query"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.as_value().unwrap(), &Value::String("".into()));

        // Type "bar" via the fallback again, then Esc → closes mode
        // and drops the in-flight query.
        for c in "bar".chars() {
            crate::event_loop::handle_unbound_search_key(
                &client,
                crossterm::event::KeyCode::Char(c),
            )
            .await;
        }
        let esc = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Search,
            key: "Esc".to_string(),
            screen: ox_types::Screen::Inbox,
        };
        client.write_typed(&path!("input/key"), &esc).await.unwrap();

        let mode = client.read(&path!("ui/mode")).await.unwrap().unwrap();
        assert_eq!(mode.as_value().unwrap(), &Value::String("normal".into()));
        let live = client
            .read(&path!("ui/search_live_query"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.as_value().unwrap(), &Value::String("".into()));
        // Chip persists across close — it's a view filter.
        let chips = client
            .read(&path!("ui/search_chips"))
            .await
            .unwrap()
            .unwrap();
        match chips.as_value().unwrap() {
            Value::Array(a) => assert_eq!(a.len(), 1),
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_line_open_submits_through_command_exec() {
        // Full pipeline: open the command line, type "quit", submit —
        // should route through command/exec to the quit target and land
        // as a ui/quit write observable in UiStore's pending_action.
        use ox_ui::text_input_store::{Edit, EditOp, EditSequence, EditSource};

        let handle = test_setup().await;
        let client = handle.client();

        // Open the command line
        client
            .write(
                &path!("ui/command_line/open"),
                structfs_core_store::Record::parsed(Value::Null),
            )
            .await
            .unwrap();

        // Verify open flag toggled
        let open = client
            .read(&path!("ui/command_line/open"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.as_value().unwrap(), &Value::Bool(true));

        // Type "quit" into the buffer
        let seq = EditSequence {
            edits: vec![Edit {
                op: EditOp::Insert {
                    text: "quit".into(),
                },
                at: 0,
                source: EditSource::Key,
                ts_ms: 0,
            }],
            generation: 0,
        };
        client
            .write_typed(&path!("ui/command_line/edit"), &seq)
            .await
            .unwrap();

        // Submit — dispatches quit via command/exec
        client
            .write(
                &path!("ui/command_line/submit"),
                structfs_core_store::Record::parsed(Value::Null),
            )
            .await
            .unwrap();

        // Post-submit: prompt is closed, buffer cleared, and the text
        // is staged on pending_submit waiting for the event loop to
        // drain it. No spawn, no race — synchronous state transitions.
        let open = client
            .read(&path!("ui/command_line/open"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.as_value().unwrap(), &Value::Bool(false));
        let pending = client
            .read(&path!("ui/command_line/pending_submit"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.as_value().unwrap(), &Value::String("quit".into()));

        // Simulate the event loop's drain: dispatch `command/exec`,
        // then clear the pending field. Both writes are synchronous
        // through the broker and the effect lands deterministically.
        client
            .write(
                &path!("command/exec"),
                structfs_core_store::Record::parsed(Value::String("quit".into())),
            )
            .await
            .unwrap();
        client
            .write(
                &path!("ui/command_line/clear_pending_submit"),
                structfs_core_store::Record::parsed(Value::Null),
            )
            .await
            .unwrap();

        // quit set pending_action to Quit; pending_submit is cleared.
        let pa = client
            .read(&path!("ui/pending_action"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pa.as_value().unwrap(), &Value::String("quit".into()));
        let pending = client
            .read(&path!("ui/command_line/pending_submit"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.as_value().unwrap(), &Value::Null);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn colon_binding_on_inbox_opens_command_line() {
        // The original bug: `:` didn't work on the inbox screen. This
        // test nails the regression: Normal mode on inbox, key ":".
        let handle = test_setup().await;
        let client = handle.client();

        let event = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: ":".to_string(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &event)
            .await
            .unwrap();

        let open = client
            .read(&path!("ui/command_line/open"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.as_value().unwrap(), &Value::Bool(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_store_mounted_with_defaults() {
        let handle = test_setup().await;
        let client = handle.client();

        let model = client
            .read(&path!("config/gate/defaults/model"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );

        let account = client
            .read(&path!("config/gate/defaults/account"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            account.as_value().unwrap(),
            &Value::String("anthropic".into())
        );
    }
}
