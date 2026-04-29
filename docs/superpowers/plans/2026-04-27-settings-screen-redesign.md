# Settings screen redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the bespoke flat-pane settings screen with a path-based MVU implementation: typed `View` tree returned by pure renderers, commands as Rust trait impls, subscriptions as a broker-side protocol over StructFS. Settings is the first user of the pattern.

**Architecture:** Three trees (data, display, view). A *cursor* path at `ui/settings/cursor` selects the active page; a *renderer registry* (`HashMap<Path, Box<dyn Renderer>>`) dispatches by literal path; renderers are pure `&dyn Reader -> View` functions; commands are pure `&dyn Reader -> Vec<Write>` Rust trait impls; long-running effects are subscriptions registered against `PathPattern`s in the broker, intercepted by `DispatchingStore`. Identifiers from outside our control (model ids) are values, never path components.

**Tech Stack:** Rust 2021; ratatui (TUI rendering, isolated to one translator module); tokio (async runtime, `AbortHandle`); serde + `structfs_serde_store` (typed cross-boundary records); `structfs_core_store` (Reader/Writer/Store + `Cascade<A,B>` overlay); insta (snapshot tests, both View-struct and ratatui-buffer); `oxpath!` macro for typed path construction; `PathComponent::try_new` for component validation.

**Spec:** `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md` — read it before starting any task.

**Plan organization.** Phases A–C build the typed records. Phase D adds the View tree. Phase E ships the View→ratatui translator. Phase F builds the subscription protocol in `ox-broker`. Phases G–H add the renderer/command/binding registries. Phase I builds the snapshot. Phases J–K implement renderers. Phases L–M register day-one commands and bindings. Phase N writes the settings subscriptions in `ox-gate`. Phases O–P do kernel/dispatch wiring. Phase Q populates index entries and handles first-run. Phases R–S verify and clean up. The phases of this plan are sized for one task per session/subagent dispatch.

**Plan style.** Tasks state file paths, function signatures, the test that goes first (TDD), and the commit message. Code samples are illustrative, not transcribed implementations — when reality (lifetime quirks, real API names) differs, the engineer adjusts. The plan's durable parts are the path conventions, the type shapes, and the test cases.

---

## Phase A — Type relocation prerequisite

### Task A0: Relocate API keys into StructFS

**Files:**
- Create: `crates/ox-gate/src/api_key.rs` — `pub struct ApiKey(pub String)` with serde.
- Modify: `crates/ox-gate/src/lib.rs` — re-export.
- Modify: every site that reads/writes key files via filesystem helpers (today: `crate::config::resolve_keys`, the test-connection path, the save-to-disk path).
- Modify: the persistence layer (likely in `ox-cli` or `ox-gate`) — split `secret/*` subtree into a separate file (`keys.json`, `chmod 0600`) at save time; the rest of `config/*` continues to a different file.
- Add: a one-shot startup migration that reads legacy on-disk key files into `secret/keys/{account}: ApiKey` if `secret/keys/*` is empty.

- [ ] **Step 1:** Add `ApiKey(String)` newtype with serde + a small round-trip test.
- [ ] **Step 2:** Replace the existing `read_key_file(account)` / `write_key_file(account, key)` callers with `read_typed::<ApiKey>(secret/keys/{account})` / `write_typed`. Tests at each callsite stay the same shape; their fixture setup writes to the namespace instead of the filesystem.
- [ ] **Step 3:** Update persistence: when the save subscription runs, separate the `secret/*` subtree from `config/*`, serialize each to its own file. Set `0600` permissions on `keys.json`.
- [ ] **Step 4:** Migration: at startup, before the first frame, if `secret/keys/*` is empty AND legacy key files exist on disk, read each and write through the broker. Log "migrated N legacy key files into namespace."
- [ ] **Step 5:** `cargo test --workspace` PASS.
- [ ] **Step 6:** Commit `refactor(gate): relocate API keys into StructFS at secret/keys/{account}`.

This task lands first because it simplifies AccountDeleteSubscription (Task N5) and removes the only seam between subscription handlers and the filesystem.

### Task A1: Move `ModelInfo` from `ox-kernel` to `ox-gate`

**Files:**
- Modify: `crates/ox-kernel/src/lib.rs:208-215` (remove `ModelInfo` struct + `// Model catalog` divider)
- Create: `crates/ox-gate/src/model_info.rs` (new home for `ModelInfo` and `ModelInfoSource`, with the extended fields per spec §5.1)
- Modify: `crates/ox-gate/src/lib.rs` (declare module + re-export; drop `use ox_kernel::ModelInfo`)
- Search-and-modify: every file importing `ox_kernel::ModelInfo` — switch to `ox_gate::ModelInfo`

- [ ] **Step 1:** `rg "ox_kernel::.*ModelInfo" crates/` to enumerate import sites. Likely: `ox-cli/src/transport.rs`, `ox-cli/src/settings_state.rs`, `ox-gate/src/lib.rs`, plus tests.
- [ ] **Step 2:** Create `model_info.rs` with the full extended `ModelInfo` (`id`, `display_name`, `max_context_size: Option<u32>`, `max_output_tokens: Option<u32>`, `source: ModelInfoSource`) and the `ModelInfoSource { Server, KnownTable, UserOverride }` enum, deriving `Debug, Clone, Serialize, Deserialize`; `PartialEq, Eq` on the enum.
- [ ] **Step 3:** Add `pub mod model_info; pub use model_info::{ModelInfo, ModelInfoSource};` to `ox-gate/src/lib.rs`. Remove the existing `use ox_kernel::ModelInfo`.
- [ ] **Step 4:** Delete `ModelInfo` from `crates/ox-kernel/src/lib.rs`.
- [ ] **Step 5:** Fix every import site found in Step 1.
- [ ] **Step 6:** Add a serde round-trip test inside `model_info.rs` covering both struct fields and enum variants.
- [ ] **Step 7:** `cargo check --workspace` clean; `cargo test --workspace` clean.
- [ ] **Step 8:** Commit.

```
refactor(gate): relocate and extend ModelInfo

Catalog metadata is gate-domain (providers, accounts, catalogs); the
kernel reads (model_id, max_output_tokens) as primitives at request
time and never imports the struct. Adds max_context_size,
max_output_tokens, ModelInfoSource for the new resolution-order story.
```

---

## Phase B — New typed records in `ox-gate`

Each task is one record, one module, serde round-trip test, one commit.

### Task B1: `CompletionRole`

**Files:** create `crates/ox-gate/src/completion_role.rs`; modify `lib.rs`.

- [ ] **Step 1:** Define `pub struct CompletionRole { pub account: String, pub model_id: String }` with `Debug, Clone, Serialize, Deserialize, PartialEq, Eq`.
- [ ] **Step 2:** Serde round-trip test.
- [ ] **Step 3:** Add `pub mod completion_role; pub use completion_role::CompletionRole;` to `lib.rs`.
- [ ] **Step 4:** `cargo test -p ox-gate completion_role::tests` PASS.
- [ ] **Step 5:** Commit `feat(gate): add CompletionRole typed record`.

### Task B2: `AccountTestStatus`

**Files:** create `crates/ox-gate/src/account_test_status.rs`; modify `lib.rs`.

- [ ] **Step 1:** Define the four-variant enum per spec §5.4 with `Debug, Clone, Serialize, Deserialize, PartialEq, Eq`.
- [ ] **Step 2:** Round-trip every variant.
- [ ] **Step 3:** Re-export.
- [ ] **Step 4:** Test PASS.
- [ ] **Step 5:** Commit `feat(gate): add AccountTestStatus typed record`.

### Task B3: `CatalogRefreshStatus`

Same shape as B2.

- [ ] **Step 1:** Define per spec §5.4.
- [ ] **Step 2:** Round-trip every variant.
- [ ] **Step 3:** Re-export.
- [ ] **Step 4:** Test PASS.
- [ ] **Step 5:** Commit `feat(gate): add CatalogRefreshStatus typed record`.

### Task B4: `KnownFamilyEntry` and the lookup table

**Files:** create `crates/ox-gate/src/known_family.rs`; modify `lib.rs`.

- [ ] **Step 1:** Write tests first. Cases: `claude-sonnet-4-*` (Anthropic dialect) → `Some(200_000, ≥32_000)`; `claude-haiku-4-5-*` → `Some(200_000, 8192)`; `gpt-4o*` (OpenAI dialect) → `Some(128_000, _)`; unknown id → `None`; dialect disambiguates overlapping prefixes. **Add a longest-prefix-wins test:** with both `claude-` and `claude-haiku-4-5` rules in the table, an id of `claude-haiku-4-5-20251001` resolves to the haiku-specific rule, not the generic `claude-` rule.
- [ ] **Step 2:** Implement: `pub struct KnownFamilyEntry { max_context_size: Option<u32>, max_output_tokens: Option<u32> }`, a private `FamilyRule { dialect, prefix, entry }` struct, a `const FAMILY_TABLE: &[FamilyRule]` covering Claude 3.x/4.x/4.5, GPT-4o, GPT-4 turbo, Llama 3.x. Hand-order the table so longer prefixes come first within each dialect (e.g. `claude-haiku-4-5` before `claude-haiku`, before `claude-`). The lookup short-circuits on first match, so ordering is the disambiguation. Add a `debug_assert!` in a one-time validator (`#[test] fn family_table_is_longest_prefix_first()`) that asserts within each dialect, prefixes are sorted by `len()` descending — guards against future additions inserting in the wrong place.
- [ ] **Step 3:** Re-export.
- [ ] **Step 4:** All tests PASS.
- [ ] **Step 5:** Commit `feat(gate): add known-family fallback table for model tokens`.

---

## Phase C — Typed records in `ox-types`

These records cross the broker boundary. Each task: one module file, serde tests, one commit.

### Task C1: Settings UI records

**Files:** create `crates/ox-types/src/settings.rs`; modify `lib.rs`.

Records to define (per spec §5.6):
- `AccountField`, `ModelField` enums (`Hash, Eq, PartialEq, Copy` plus serde).
- `ModelKey { account, model_id }` (`Hash, Eq, PartialEq, Clone` plus serde).
- `SettingsIndexEntry { id, label, description, target_cursor: Path, badge: BadgeSource }`.
- `BadgeSource { None, Static(String), SubtreeCount(Path), PrimaryReference }`.
- `ValidationDiagnostics { field_errors: BTreeMap<AccountField, String>, computed_at_ms: u64 }`.
- `GlobalBanner { None, Error{...}, Info{...} }`.

- [ ] **Step 1:** Define all the records with the above derives.
- [ ] **Step 2:** Serde round-trip each (one test per record/variant set).
- [ ] **Step 3:** `pub mod settings; pub use settings::*;` in `lib.rs`.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(types): add settings UI records`.

### Task C2: Key chord types

**Files:** create `crates/ox-types/src/key_chord.rs`.

- [ ] **Step 1:** Define `KeyChord`, `KeyModifierSet`, `KeyCodeRepr` per spec §5.6 with `Clone, Debug, PartialEq, Eq, Hash` plus serde. `Default` on `KeyModifierSet`.
- [ ] **Step 2:** Round-trip a representative set: bare char, ctrl+char, esc, F-keys, arrows.
- [ ] **Step 3:** Helper `KeyChord::from_crossterm(KeyEvent) -> KeyChord` (and reverse if needed by ratatui input plumbing — check existing call sites).
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(types): add KeyChord typed key representation`.

### Task C3: Command and binding registry types

**Files:** create `crates/ox-types/src/command_binding.rs`.

Records:
- `CommandId(String)` — `Hash, Eq, PartialEq, Clone, Debug` + serde.
- `CommandDisplay { name: String, description: String }`.
- `CommandScope { screen: Screen, cursor_path: Option<Path> }`.
- `BindingEntry { screen: Screen, cursor_path: Option<Path>, mode: Option<Mode>, key: KeyChord, command_id: CommandId }`.

NOTE: this task ships **only** the data-shaped types. There is no `CommandEffect`, `PathTemplate`, `PayloadSource`. The trait `Command` lives in `ox-cli` (Phase H).

- [ ] **Step 1:** Define each record with derives.
- [ ] **Step 2:** Serde round-trip tests.
- [ ] **Step 3:** Re-export.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(types): add CommandId/CommandScope/BindingEntry typed records`.

### Task C4: Subscription protocol records

**Files:** create `crates/ox-types/src/subscription.rs`.

Records (per spec §5.8 — the data shapes; the traits live in `ox-broker` because they reference `Reader`/`Store`):

- `SubscriptionId(String)` — `Clone, Debug, Eq, PartialEq, Hash` + serde.
- `PathPattern { Exact(Path), Prefix(Path), PrefixSuffix { prefix: Path, suffix: Path } }` — `Clone, Debug, Eq, PartialEq` + serde. Method `pub fn matches(&self, path: &Path) -> bool`. `PrefixSuffix` matches paths whose components start with `prefix` AND end with `suffix`, with at least one component between them.
- `PathChange { path: Path, before: Option<Record>, after: Option<Record> }` — derive `Clone, Debug` (no serde required; this is in-process only).
- `Write { path: Path, record: Record }` — `Clone, Debug`.

- [ ] **Step 1:** Define each. `suffix` is a `Path`, not `Vec<String>` — components are validated through the same `PathComponent::try_new` boundary.
- [ ] **Step 2:** Tests for `PathPattern::matches`:
    - Exact match; non-match.
    - Prefix matches `config/gate/accounts/foo`; does NOT match `config/gate/accounts_other/foo` (component-level boundary, not byte-level).
    - PrefixSuffix `{ prefix: config/gate/accounts, suffix: test_now }` matches `config/gate/accounts/foo/test_now` and `config/gate/accounts/anthropic-personal/test_now`; does NOT match `config/gate/accounts/test_now` (no instance segment), `config/gate/accounts/foo/refresh_now` (wrong suffix), `config/gate/accounts/foo/bar/test_now` (matches — multi-segment suffix, single instance is fine; verify the spec's "at least one component between" interpretation in the impl — specifically: prefix and suffix must not overlap, but the gap can be ≥1 segments. Tests cover both cases).
    - Empty path edge case.
- [ ] **Step 3:** Serde round-trip for `SubscriptionId` and `PathPattern` (BindingEntry-adjacent records may reference these).
- [ ] **Step 4:** Re-export.
- [ ] **Step 5:** Commit `feat(types): add subscription protocol records`.

---

## Phase D — The View tree (new `ox-view` crate)

The View enum is the typed output of every renderer. New crate with no ratatui dependency.

### Task D1: Create `ox-view` crate

**Files:**
- Create: `crates/ox-view/Cargo.toml`
- Create: `crates/ox-view/src/lib.rs`
- Modify: workspace `Cargo.toml` (`members += ["crates/ox-view"]`)

- [ ] **Step 1:** Initialize the crate. **No** dependencies on `serde` or `ratatui` — `View` is in-memory only in v1, and serializing it is forward-compat work for v2 (Rio at the widget level). Keeping the crate dep-free makes the build graph cleaner and prevents accidental wire-protocol coupling.
- [ ] **Step 2:** Define the View enum and supporting types per spec §5.7. Derive `Debug, Clone, PartialEq` on every type so renderer tests can compare with `assert_eq!`. Note: `StatusBlock` carries `scroll_offset: u16` (the renderer reads the offset from a path and passes it through; the translator uses it to position the visible window), not `scrollable: bool` — otherwise scrolling either requires translator state or doesn't work.
- [ ] **Step 3:** Add small constructors that reduce noise in renderers: `View::text(s)`, `View::stack_v(children)`, `View::stack_h(children)`, `View::pad(view, padding)`.
- [ ] **Step 4:** Add `View::unknown_cursor_fallback(cursor: &Path) -> View` — returns a `Stack` with a `Banner::Error`-styled message and an instruction line.
- [ ] **Step 5:** Sanity test: build a small View by hand and assert structural equality.
- [ ] **Step 6:** `cargo check -p ox-view`, `cargo test -p ox-view`.
- [ ] **Step 7:** Commit `feat(view): add ox-view crate with curated View enum`.

### Task D2: View constructor coverage tests

- [ ] **Step 1:** For each variant of `View`, construct an example. Assert structural equality. Confirms `PartialEq` is meaningful and the constructors compile cleanly.
- [ ] **Step 2:** Specifically test `View::Modal { background, foreground, dim }` composes with two non-trivial sub-views.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `test(view): variant coverage and modal composition`.

---

## Phase E — View → ratatui translator

The translator is the only place ratatui is touched. Lives in `ox-cli` so it can depend on the project's ratatui version directly.

### Task E1: Translator skeleton + tests

**Files:**
- Create: `crates/ox-cli/src/view_render.rs`
- Modify: `crates/ox-cli/src/lib.rs`

- [ ] **Step 1:** Public API: `pub fn render_to_frame(view: &View, frame: &mut Frame, area: Rect, theme: &Theme)`. Total over the View enum (every variant matched).
- [ ] **Step 2:** Implement variant-by-variant. `Empty` no-ops; `Text` renders a `Paragraph`; `Stack` lays out children with `Layout::default().direction(...).constraints(...)` derived from `Sizing` of each child; `List` renders ratatui's `List` + `ListState`; `Form` renders rows as labeled paragraphs with focused-row underline; `Modal` renders background then a centered `area` with the foreground (use existing helper if any; otherwise add a small `centered_rect` fn); `Banner` is a one-line bordered `Paragraph` with `BannerKind`-styled background; `StatusBlock` is a scrollable `Paragraph` with title; `Pad` insets `area` and recurses.
- [ ] **Step 3:** Snapshot tests using `ratatui::backend::TestBackend` + `insta::assert_snapshot!` of the formatted buffer. One test per variant. Format helper `fn format_buffer(buf) -> String` shared across tests.
- [ ] **Step 4:** Accept snapshots.
- [ ] **Step 5:** Commit `feat(cli): View→ratatui translator with per-variant snapshot tests`.

NOTE: keep the translator dumb. No conditional logic about *which* variant to render based on data; that's a renderer concern. The translator only knows how to draw a `View`.

---

## Phase F — Subscription protocol in `ox-broker`

The broker gains a `DispatchingStore` that wraps the underlying `Store`, intercepting writes to invoke matching subscriptions.

### Task F1: `Subscription` trait, `SubCtx`, `SpawnHandle`, `AsyncWriter`

**Files:**
- Create: `crates/ox-broker/src/subscription.rs`
- Modify: `crates/ox-broker/src/lib.rs`
- Modify: `crates/ox-broker/Cargo.toml` (add `futures` if not present for `BoxFuture`)

- [ ] **Step 1:** Trait shapes per spec §5.8 / §3.3:
    - `Subscription` with `id`, `watches`, `handle(ctx) -> Vec<Write>`.
    - `SubCtx<'a> { snapshot, change, spawn, writer }`.
    - `SpawnHandle::spawn(BoxFuture<'static, ()>) -> AbortHandle`.
    - `AsyncWriter::write(path, record) -> BoxFuture<Result<Path, StoreError>>`.
- [ ] **Step 2:** Sanity test: a no-op `Subscription` impl that returns `vec![]`; round-trip `SubscriptionId`.
- [ ] **Step 3:** Commit `feat(broker): subscription protocol traits`.

### Task F2: `SubscriptionRegistry`

**Files:** modify `crates/ox-broker/src/subscription.rs`.

- [ ] **Step 1:** Tests:
    - `register` adds entries indexed by every pattern in `watches()`.
    - `matching` returns subscriptions whose pattern matches.
    - Multiple patterns on one subscription work (the Arc is cloned per pattern).
    - Registration order is stable.
- [ ] **Step 2:** Implement `SubscriptionRegistry { entries: Vec<(PathPattern, Arc<dyn Subscription>)> }` with `register`, `matching(path)`. Linear scan; documented as fine for tens of subscriptions.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(broker): SubscriptionRegistry`.

### Task F3: `DispatchingStore`

**Files:** create `crates/ox-broker/src/dispatching_store.rs`; modify `lib.rs`.

The wrapper Store. On each write: read `before`, perform the write, build `PathChange`, find matching subscriptions, invoke each with a snapshot reader and a back-channel writer, recursively apply returned writes with depth+1, panic-catch around each handler.

- [ ] **Step 1:** Tests first (in `dispatching_store.rs`):
    - **Basic dispatch.** Subscription on `Exact(p)` returns one write to `p2`. Writing `p` causes both `p` and `p2` to land.
    - **Cascade depth.** Subscription writes back to its own watched path. Cap = 4 (configured in test). Final write attempts to recurse 5; bound triggers, error logged, last write succeeds.
    - **Panic isolation.** Two subscriptions on `Exact(p)`; first panics, second writes to `p2`. Original `write(p)` returns Ok; `p2` lands.
    - **Write ordering.** Subscription returns `[Write A, Write B]`. Storage observes A then B.
    - **Pattern boundary.** `Prefix(config/gate/accounts)` does NOT fire on `config/gate/accounts_other/foo`.
    - **Spawn lifecycle.** Subscription spawns a 50ms task that writes back. `write()` returns immediately (no wait). After ~100ms, the spawned write has landed.
    - **Sibling failure.** Subscription returns a write to a path the underlying Store rejects. Sibling subscriptions still run; original `write()` returns Ok.
- [ ] **Step 2:** Implement `DispatchingStore` per spec §3.3 / §4 of the subscription-protocol description. `write_at_depth(path, record, depth)` is the recursive core. Uses `std::panic::catch_unwind` with `AssertUnwindSafe` around `Subscription::handle`.
- [ ] **Step 3:** Implement the back-channel `SelfWriter` (an `AsyncWriter` that forwards to `DispatchingStore::write` at depth 0 — spawned task writes are new logical events).
- [ ] **Step 4:** Implement a `TokioSpawnHandle` (production `SpawnHandle` impl that calls `tokio::spawn`).
- [ ] **Step 5:** Implement a `MockSpawn` for tests. Records each `(BoxFuture, AbortHandle)` pair into a `Vec`. Exposes `pub fn drain(&self) -> Vec<(BoxFuture, AbortHandle)>` so tests can poll futures and assert on their handles. Critically, the `AbortHandle` returned to the subscriber is real (created via `tokio::task::AbortHandle::new` or by spawning the task immediately and exposing its handle) — supersession tests in N3 need to call `is_aborted()` (or check post-abort behavior) on the *prior* AbortHandle after a second trigger fires. Without this, the supersession test can't actually verify the abort happened, only that the second task's writes appear.
- [ ] **Step 6:** All tests PASS.
- [ ] **Step 7:** Commit `feat(broker): DispatchingStore with cascade-bound, panic-isolation, spawn lifecycle`.

### Task F4: Wire `DispatchingStore` into `Broker`

**Files:** modify `crates/ox-broker/src/broker.rs`, `lib.rs`, `client.rs` if needed.

- [ ] **Step 1:** `Broker::new` (or its closest equivalent) accepts `Arc<SubscriptionRegistry>` and `Arc<dyn SpawnHandle>` (with sensible defaults: empty registry, `TokioSpawnHandle`).
- [ ] **Step 2:** The internal `Store` is wrapped: `let store = DispatchingStore::new(inner, subs, spawn, /* cascade_bound */ 64);`.
- [ ] **Step 3:** `Broker::register_subscription(Arc<dyn Subscription>)` exposes registration. (For convenience, also `register_subscriptions(impl IntoIterator<Item = Arc<dyn Subscription>>)`.)
- [ ] **Step 4:** Existing tests continue to pass — the public `Broker::write` API is unchanged.
- [ ] **Step 5:** Commit `feat(broker): wire DispatchingStore into Broker; expose register_subscription`.

---

## Phase G — Renderer registry primitive

The renderer registry is `ox-cli`-local — it doesn't cross the broker boundary.

### Task G1: `Renderer` trait, `RendererRegistry`, `RenderCtx`, `AscendRule`

**Files:**
- Create: `crates/ox-cli/src/settings/mod.rs`
- Create: `crates/ox-cli/src/settings/registry.rs`
- Modify: `crates/ox-cli/src/lib.rs`

- [ ] **Step 1:** `mod.rs` declares `pub mod registry; pub mod renderers; pub mod commands; pub mod bindings; pub mod subscription_install; pub mod snapshot;` (most are stubs filled in later phases).
- [ ] **Step 2:** `registry.rs` defines:
    - `enum AscendRule { NearestRegistered, ExitScreen }`.
    - `trait Renderer: Send + Sync { fn render(&self, ctx: &RenderCtx) -> View; fn ascend_to(&self) -> AscendRule; }`.
    - `struct RenderCtx<'a> { area: Rect, data: &'a dyn Reader, registry: &'a RendererRegistry, theme: &'a Theme }`.
    - `struct RendererRegistry { specs: HashMap<Path, Box<dyn Renderer>> }` with `register`, `lookup`, `render(cursor, ctx) -> View` (returns `View::unknown_cursor_fallback(cursor)` on miss), `ascend(cursor) -> Option<Path>` per spec §4.1.
- [ ] **Step 3:** Tests:
    - `ascend_exit_screen_returns_none`.
    - `ascend_nearest_registered_walks_to_parent`.
    - `ascend_skips_unregistered_intermediate`.
    - `render_unknown_cursor_returns_fallback_view` (assert the View is the fallback shape).
- [ ] **Step 4:** `cargo check -p ox-cli`, `cargo test -p ox-cli settings::registry::tests` PASS.
- [ ] **Step 5:** Commit `feat(cli): renderer registry primitive`.

`Path::parent` may not exist by that name in `structfs_core_store::Path`; if not, implement `nearest_registered_parent` by manually splitting the path string and reconstructing.

---

## Phase H — Command and binding registries

### Task H1: `Command` trait, `CommandRegistry`

**Files:** create `crates/ox-cli/src/settings/command_registry.rs`.

- [ ] **Step 1:** Define:
    ```rust
    pub trait Command: Send + Sync {
        fn id(&self)      -> &CommandId;
        fn display(&self) -> &CommandDisplay;
        fn scope(&self)   -> &CommandScope;
        fn run(&self, snapshot: &dyn Reader, ctx: &CommandCtx<'_>) -> Vec<Write>;
    }

    pub struct CommandCtx<'a> {
        pub registry:       &'a RendererRegistry,
        pub last_keystroke: Option<KeyChord>,
    }

    pub struct CommandRegistry { by_id: HashMap<CommandId, Box<dyn Command>> }
    ```
    With `register`, `lookup`, `iter`.
- [ ] **Step 2:** A trivial test command (returns one literal Write) registers and looks up correctly. Plus a test command that reads `ctx.registry` (verifies the field is wired) and one that reads `ctx.last_keystroke` (verifies optional handling).
- [ ] **Step 3:** Commit `feat(cli): Command trait + CommandCtx + CommandRegistry`.

### Task H2: `BindingRegistry`

**Files:** create `crates/ox-cli/src/settings/binding_registry.rs`.

- [ ] **Step 1:** Define `BindingRegistry { entries: Vec<BindingEntry> }` with `register` (sort-by-specificity-keep-stable on every insertion) and `lookup(screen, cursor, mode, key) -> Option<&CommandId>` per spec §4.5.
- [ ] **Step 2:** Tests:
    - `cursor_specific_beats_whole_screen`.
    - `mode_specific_beats_unspecified` (when both have same cursor scope).
    - `falls_through_to_whole_screen` when no specific match.
    - `registration_order_breaks_ties`.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(cli): BindingRegistry with specificity ordering`.

### Task H3: Dispatch function

**Files:** modify or create `crates/ox-cli/src/settings/dispatch.rs`.

- [ ] **Step 1:** `pub fn dispatch_settings_key(snapshot: &dyn Reader, screen: Screen, cursor: &Path, mode: Option<Mode>, key: &KeyChord, cmds: &CommandRegistry, bindings: &BindingRegistry, renderers: &RendererRegistry) -> Vec<Write>`.
    - Lookup binding → command id → command. If any step misses, return empty vec (inert).
    - Build `CommandCtx { registry: renderers, last_keystroke: Some(key.clone()) }`.
    - Call `command.run(snapshot, &ctx)`. Return its writes.
- [ ] **Step 2:** Test: register a binding+command pair; call dispatch; assert returned writes.
- [ ] **Step 3:** Test: missing binding returns empty.
- [ ] **Step 4:** Test: a command that reads `ctx.last_keystroke` sees the dispatched key.
- [ ] **Step 5:** Tests PASS.
- [ ] **Step 6:** Commit `feat(cli): settings dispatch function`.

---

## Phase I — Snapshot builder

### Task I1: `fetch_settings_view_state`

**Files:** modify `crates/ox-cli/src/settings/snapshot.rs`.

- [ ] **Step 1:** Define `SettingsSnapshot` wrapping the codebase's existing in-memory `Store` (look at `crates/ox-store-util/src/local_config.rs` and `crates/ox-store-util/src/cascade.rs`'s test pattern; `LocalConfig` is the standard in-memory Store). Implement `Reader` for `SettingsSnapshot` by forwarding to the inner store.
- [ ] **Step 2:** `pub async fn fetch_settings_view_state(client: &ClientHandle) -> SettingsSnapshot` walks these prefixes via the broker client and inserts each `(path, record)` into the snapshot's inner store:
    - `config/gate/accounts`
    - `config/gate/providers`
    - `config/completions`
    - `ui/settings`
    - `ui/global`
    - `settings/index/entries`
- [ ] **Step 3:** Use the existing subtree-walk helper from the broker client. Check `crates/ox-broker/src/client.rs` for the actual method name (likely `read_subtree` or `walk`); if missing, add a minimal implementation that lists then reads each leaf.
- [ ] **Step 4:** Smoke test: write `CompletionRole` to a real broker, fetch snapshot, read back via `Reader::read_typed`. Asserts the right value.
- [ ] **Step 5:** Tests PASS.
- [ ] **Step 6:** Commit `feat(cli): settings pre-render snapshot builder`.

---

## Phase J — Settings renderers

Each renderer: one module, `impl Renderer`, `register(&mut RendererRegistry)`, snapshot tests over `View` struct, one commit.

### Task J1: `IndexRenderer`

**Files:** create `crates/ox-cli/src/settings/renderers/index.rs`.

The renderer reads `settings/index/entries/*` (typed `SettingsIndexEntry`), `ui/settings/index/selected: usize`. Resolves each entry's badge synchronously (`SubtreeCount` reads the prefix's child count from the snapshot; `PrimaryReference` reads `config/completions/primary`). Emits `View::List` with one `ListItem` per entry.

- [ ] **Step 1:** Test fixtures: build `SettingsSnapshot` with two index entries (Accounts/Models) and a selected index of 0; populate badges' source data so the resolution returns expected strings.
- [ ] **Step 2:** Test: call `renderer.render(ctx)`; assert the returned `View` has the expected `List { items, selected }` shape. Use `assert_eq!` on the View structure (write the expected View by hand — verbose but clear) **OR** `insta::assert_yaml_snapshot!` (saves bulk).
- [ ] **Step 3:** Test variants: empty entries; non-zero selected; `SubtreeCount` resolves to 0 when prefix is empty.
- [ ] **Step 4:** Implement the renderer. `ascend_to(&self) -> AscendRule { AscendRule::ExitScreen }`.
- [ ] **Step 5:** `pub fn register(reg: &mut RendererRegistry)` registers at `oxpath!("settings", "index")`.
- [ ] **Step 6:** Tests PASS.
- [ ] **Step 7:** Commit `feat(cli): IndexRenderer returns View::List of categories`.

### Task J2: `AccountsListRenderer`

**Files:** create `crates/ox-cli/src/settings/renderers/accounts_list.rs`.

Reads: `config/gate/accounts/*`, `config/gate/providers/{ref}`, `secret/keys/{name}: ApiKey` (presence drives the `✓key`/`–` indicator; reads via the broker — `crate::config::resolve_keys` is gone after Task A0), `ui/settings/accounts/selected: Option<String>`.

Emits: `View::List` with rows showing name, resolved provider host, key indicator (`✓key`/`–`).

- [ ] **Step 1:** Fixtures: zero, one, three accounts; with/without selection; with/without keys.
- [ ] **Step 2:** Tests: `accounts_list_empty`, `accounts_list_three`, `accounts_list_with_selection`.
- [ ] **Step 3:** Implement. `ascend_to = NearestRegistered`.
- [ ] **Step 4:** Register at `oxpath!("settings", "accounts")`.
- [ ] **Step 5:** Tests PASS.
- [ ] **Step 6:** Commit `feat(cli): AccountsListRenderer`.

### Task J3: `AccountDetailRenderer`

**Files:** create `crates/ox-cli/src/settings/renderers/account_detail.rs`.

Reads:
- `ui/settings/accounts/selected: Option<String>` — handle `None` with empty-state `View::Text`.
- `config/gate/accounts/{selected}` (if present); the provider; `secret/keys/{selected}: ApiKey` (read via the broker; the filesystem helper is gone after Task A0); `…/test_status`; `…/refresh_status`; `…/validation_status`.
- `ui/settings/account_detail/field: AccountField` and `ui/settings/edit_cursor: u32`.

Emits: `View::Stack { dir: Vertical, children: [Form, StatusBlock] }`.

- [ ] **Step 1:** Fixtures + tests:
    - `account_detail_no_selection` (View is empty-state Text).
    - `account_detail_valid` (Form + idle StatusBlock).
    - `account_detail_with_test_failure` (StatusBlock contains the failure reason).
    - `account_detail_with_validation_errors` (Form rows have `error: Some(...)`).
    - `account_detail_during_test` (StatusBlock shows "Testing…").
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Register at `oxpath!("settings", "accounts", "_detail")`.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(cli): AccountDetailRenderer`.

### Task J4: `ModelsListRenderer`

**Files:** create `crates/ox-cli/src/settings/renderers/models_list.rs`.

Reads: every `config/gate/accounts/{name}/models: Vec<ModelInfo>`; `config/completions/primary: Option<CompletionRole>`; `ui/settings/models/selected: Option<ModelKey>`; per-account `refresh_status` for chrome.

Flattens to one row per `(account, model_id)`. Columns surface in `ListItem.primary` and `secondary`/`badge` fields; the source-tag and primary-tag both go in the badge column joined.

- [ ] **Step 1:** Fixtures + tests: empty, three accounts, primary tagged, unknown token fields, refresh chrome.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Register at `oxpath!("settings", "models")`.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(cli): ModelsListRenderer (unified per-account browser)`.

### Task J5: `ModelDetailRenderer`

**Files:** create `crates/ox-cli/src/settings/renderers/model_detail.rs`.

Reads `ui/settings/models/selected: Option<ModelKey>` (None → empty-state); `config/gate/accounts/{selected.account}/models`, scans for `selected.model_id`. Emits `View::Form` with id (read-only), display name (read-only), max_context_size + source badge, max_output_tokens + source badge.

- [ ] **Step 1:** Fixtures + tests: no selection, server source, known-table source, user-override, unknown fields.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Register at `oxpath!("settings", "models", "_detail")`.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(cli): ModelDetailRenderer`.

---

## Phase K — Overlay renderers

Both overlays compose via `View::Modal { background, foreground, dim: true }`.

### Task K1: New-account overlay

**Files:** create `crates/ox-cli/src/settings/renderers/overlay_new_account.rs`.

- [ ] **Step 1:** `render` calls `ctx.registry.render(&oxpath!("settings","accounts"), ctx)` for the background View, then constructs the modal foreground (a small Form with the name input).
- [ ] **Step 2:** Reads draft text from `ui/settings/new_account/name_input: String`.
- [ ] **Step 3:** Tests: empty input, partial name, valid name.
- [ ] **Step 4:** `ascend_to = NearestRegistered` (lands at `settings/accounts`).
- [ ] **Step 5:** Register at `oxpath!("settings", "accounts", "_new")`.
- [ ] **Step 6:** Tests PASS.
- [ ] **Step 7:** Commit `feat(cli): new-account overlay renderer (Modal-composed)`.

### Task K2: Delete-account overlay

**Files:** create `crates/ox-cli/src/settings/renderers/overlay_delete_account.rs`.

- [ ] **Step 1:** Background = `settings/accounts` View; foreground = a confirm box: "Delete account '{selected}'? (y/n)".
- [ ] **Step 2:** Reads `ui/settings/accounts/selected`. If `None`, foreground is "Nothing selected. Press Esc."
- [ ] **Step 3:** Tests: with selection; no selection.
- [ ] **Step 4:** Register at `oxpath!("settings", "accounts", "_delete")`.
- [ ] **Step 5:** Tests PASS.
- [ ] **Step 6:** Commit `feat(cli): delete-account overlay renderer`.

---

## Phase L — Day-one commands

Each command is a Rust struct implementing `Command`. The registry collects them at startup.

### Task L1: Highlight commands (per area, next/prev)

**Files:** create `crates/ox-cli/src/settings/commands/highlight.rs`.

For each area `index | accounts | models`, two commands: `next` and `prev`. Each:
- Reads the area's selection pointer + the live row count from the snapshot.
- Computes the next index with wrap-around. For `accounts`, the row count is the number of children of `config/gate/accounts/`. For `models`, it's the sum across all accounts. For `index`, it's the entry count.
- Returns one `Write` to the selection pointer with the new value (the index for `index`, the account name for `accounts`, the `ModelKey` for `models`).

- [ ] **Step 1:** Tests: next wraps at end; prev wraps at start; no-op on empty list.
- [ ] **Step 2:** Implement six commands as separate types:
    `HighlightIndexNext`, `HighlightIndexPrev`,
    `HighlightAccountsNext`, `HighlightAccountsPrev`,
    `HighlightModelsNext`, `HighlightModelsPrev`.
    Or a single generic + parameterized `id()`/`scope()`. Pick the form with less boilerplate; both work.
- [ ] **Step 3:** Register all six in `commands::register(&mut CommandRegistry)`.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(cli): highlight commands (per-area, next/prev) as pure Command impls`.

### Task L2: Navigation commands

**Files:** create `crates/ox-cli/src/settings/commands/navigation.rs`.

- `NavDescendIndex`: reads highlighted index entry's `target_cursor`, writes to `ui/settings/cursor`.
- `NavDescendAccounts`: writes `ui/settings/cursor ← oxpath!("settings","accounts","_detail")`.
- `NavDescendModels`: writes `ui/settings/cursor ← oxpath!("settings","models","_detail")`.
- `NavAscend`: reads `ui/settings/cursor` from the snapshot; consults `ctx.registry.ascend(&cursor)`; writes the result (or returns `vec![]` when `AscendRule::ExitScreen` and the dispatcher emits an Exit signal — see Step 1 below). No special case in the dispatcher; the registry is carried through `CommandCtx`.

`NavAscend`'s `run` returns `Vec<Write>` for the normal "write parent to cursor" path. For `ExitScreen`, the command can't directly cause the event loop to switch screens via a Write (there's no path the loop watches for that). Two options:
- (a) Reserve a path `ui/settings/_request_exit: bool` that the event loop reads on its next iteration. `NavAscend` writes `true`; the loop clears it after exiting.
- (b) Have dispatch return a richer outcome enum (`Vec<Write>` PLUS `ExitRequested`) and let the event loop branch on it.

Pick (a). It keeps `Command::run` returning `Vec<Write>` (no second outcome type), and the exit path is observable in the namespace like everything else.

- [ ] **Step 1:** Tests for each descend command (a snapshot with the relevant precondition; assert the cursor write).
- [ ] **Step 2:** Test for `NavAscend`: with cursor at a `NearestRegistered` page, asserts cursor is rewritten to parent. With cursor at `ExitScreen` page, asserts a write to `ui/settings/_request_exit: true`.
- [ ] **Step 3:** Implement and register.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(cli): navigation commands (descend + ascend via CommandCtx)`.

### Task L3: Account/model commands

**Files:** create `crates/ox-cli/src/settings/commands/account_model.rs`.

- `AccountsAdd`: writes `ui/settings/cursor ← oxpath!("settings","accounts","_new")`.
- `AccountsDeleteConfirm`: writes `ui/settings/cursor ← oxpath!("settings","accounts","_delete")`.
- `AccountsCancel`: writes `ui/settings/cursor ← oxpath!("settings","accounts")` (used by Esc on overlays + 'n' on delete confirm).
- `AccountsCreate`: reads `ui/settings/new_account/name_input: String`; if non-empty, writes typed `CreateAccountRequest { name }` to `config/gate/accounts/_create_now`. Subscription does the rest.
- `AccountsDelete`: writes `Null` to `config/gate/accounts/{selected}/delete_now` where `{selected}` comes from the snapshot.
- `AccountTest`: reads `ui/settings/accounts/selected`; if `Some(name)`, writes `Null` to `config/gate/accounts/{name}/test_now`. If `None`, returns `vec![]`.
- `AccountRefresh`: reads `ui/settings/models/selected: ModelKey`; writes `Null` to `config/gate/accounts/{selected.account}/refresh_now`.
- `ModelsSetPrimary`: reads `ui/settings/models/selected: ModelKey`; builds `CompletionRole { account, model_id }`; writes to `config/completions/primary`. Inert if selected is None.
- `AppSave`: writes `Null` to `config/save`.
- `FieldAccountNext` / `FieldAccountPrev`: read `ui/settings/account_detail/field: AccountField`; write the next/prev variant.
- `FieldModelNext` / `FieldModelPrev`: same for `ui/settings/model_detail/field`.
- `FieldInsert`: reads `ctx.last_keystroke` for the character; reads the focused field's pointer + the field's current text + `ui/settings/edit_cursor`; writes updated text + new cursor position. Inert when `last_keystroke` is `None` or the keystroke is not a `Char`.
- `FieldDeleteBack`: same shape, deletes the char before cursor.
- `SelectorCycleProtocol` / `SelectorCycleAuth`: reads the current AccountConfig, increments the selector field, writes back.

- [ ] **Step 1:** Tests, one per command. Set up snapshot with the relevant precondition, call `run`, assert writes.
- [ ] **Step 2:** Implement each command.
- [ ] **Step 3:** `register(reg)` adds them all.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(cli): day-one settings commands (account/model/field)`.

### Task L4: Top-level command registration

**Files:** modify `crates/ox-cli/src/settings/commands/mod.rs`.

- [ ] **Step 1:** `pub fn register_all(reg: &mut CommandRegistry)` calls each sub-module's `register`.
- [ ] **Step 2:** Sanity test: `register_all` populates without panic; spot-check a handful of expected ids.
- [ ] **Step 3:** Commit `feat(cli): top-level command registration`.

---

## Phase M — Day-one bindings

### Task M1: Bindings

**Files:** create `crates/ox-cli/src/settings/bindings.rs`.

Per spec §6 binding tables. Write out every entry as a literal `BindingEntry { ... }`. Do not abbreviate or use macros — flat clarity.

Entries per cursor scope:
- `settings/index`: `j`/`k` → `highlight.index.{next,prev}`; `Enter` → `nav.descend.index`; `Esc` → `nav.ascend`.
- `settings/accounts`: `j`/`k` → `highlight.accounts.{next,prev}`; `Enter` → `nav.descend.accounts`; `a` → `accounts.add`; `d` → `accounts.delete_confirm`; `Esc` → `nav.ascend`.
- `settings/accounts/_detail`: `Tab`/`Down` → `field.account.next`; `Shift+Tab`/`Up` → `field.account.prev`; `t` → `account.test`; `Ctrl+s` → `app.save`; `Esc` → `nav.ascend`; printable char keys → `field.insert`; `Backspace` → `field.delete_back`. The two char/backspace bindings use `KeyChord` patterns covering the relevant keys (single binding entry per chord; `field.insert` reads the actual character from `ctx.last_keystroke`).
- `settings/accounts/_new`: `Enter` → `accounts.create`; `Esc` → `accounts.cancel`.
- `settings/accounts/_delete`: `y` → `accounts.delete`; `n`/`Esc` → `accounts.cancel`.
- `settings/models`: `j`/`k` → `highlight.models.{next,prev}`; `Enter` → `nav.descend.models`; `P` → `models.set_primary`; `r` → `account.refresh`; `Esc` → `nav.ascend`.
- `settings/models/_detail`: `Tab`/`Down` → `field.model.next`; `Shift+Tab`/`Up` → `field.model.prev`; `Esc` → `nav.ascend`.

- [ ] **Step 1:** Write `pub fn register(reg: &mut BindingRegistry)` with every entry above.
- [ ] **Step 2:** Char-key bindings for text-editing scopes: a small helper `fn register_text_editing(reg: &mut BindingRegistry, cursor: Path)` that iterates over the printable ASCII charset (a const slice or `(0x20..=0x7E)` filtered to printable) and registers one `BindingEntry` per char → `field.insert`, plus one for `Backspace` → `field.delete_back`. ~96 entries per text-editing scope, contained to this helper. Data shape stays simple. Call it for `settings/accounts/_detail` and any future text-editing cursor scope.
- [ ] **Step 3:** Sanity test: lookup a representative key from each scope; assert the right `command_id`. Plus: lookup a printable char from `_detail` resolves to `field.insert`; lookup an unbound chord (e.g. `Ctrl+x` on `_detail`) returns `None`.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(cli): day-one settings bindings + text-editing char-key helper`.

---

## Phase N — Subscription impls in `ox-gate`

This phase implements the five day-one subscriptions. Each is a struct implementing `Subscription`. Each lives in its own module under `crates/ox-gate/src/subscriptions/`.

### Task N1: Transport relocation

**Files:**
- Move: `crates/ox-cli/src/transport.rs` → `crates/ox-gate/src/transport.rs`
- Modify: `crates/ox-gate/src/lib.rs` (declare module + re-export)
- Modify: `crates/ox-cli/src/lib.rs` (drop the module; add a re-export from `ox-gate` for any existing call sites)
- Modify: imports in `ox-cli` callers — switch to `ox_gate::transport`

Transport (the test-connection and catalog-fetch functions) is gate-domain. It was hosted in `ox-cli` for historical reasons; subscriptions in `ox-gate` need it.

- [ ] **Step 1:** Identify the public functions in `ox-cli/src/transport.rs` (likely `test_connection_async`, `fetch_model_catalog_async`, plus a transport trait if extracted).
- [ ] **Step 2:** Move the file.
- [ ] **Step 3:** Update `ox-gate/Cargo.toml` with the necessary HTTP/transport dependencies.
- [ ] **Step 4:** Fix every call site in `ox-cli`.
- [ ] **Step 5:** If a transport trait doesn't exist yet, extract one now: `pub trait Transport: Send + Sync { async fn test_connection(...); async fn fetch_catalog(...); }` with a default `HttpTransport` impl. Subscriptions take `Arc<dyn Transport>` so tests can substitute a mock.
- [ ] **Step 6:** `cargo check --workspace`, `cargo test --workspace` PASS.
- [ ] **Step 7:** Commit `refactor(transport): relocate to ox-gate; extract Transport trait for mockability`.

### Task N2: Validation helper

**Files:** create `crates/ox-gate/src/validation.rs`.

`pub fn validate_account(cfg: &AccountConfig) -> Option<ValidationDiagnostics>`. Composes existing `validate_endpoint`, `AuthScheme::requires_key`, `PathComponent::try_new` checks. Returns `None` when valid; `Some(diagnostics)` with one entry per failing field.

- [ ] **Step 1:** Tests: valid AccountConfig → None; bad endpoint → Some with `Endpoint` error; missing key when required → Some with `Key` error; multiple errors compose.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(gate): validate_account helper`.

### Task N3: `AccountTestSubscription`

**Files:** create `crates/ox-gate/src/subscriptions/account_test.rs`.

Watches: `PathPattern::PrefixSuffix { prefix: oxpath!("config","gate","accounts"), suffix: oxpath!("test_now") }`. Match precision is at the protocol level — the handler is invoked only on actual `…/test_now` writes, never on key/endpoint/etc. writes under the same prefix.

`handle`:
1. Extract `name` from `change.path` (the segment between prefix and suffix). Helper `fn instance_segment(change: &PathChange, prefix: &Path, suffix: &Path) -> Option<String>` lives in a small shared module.
2. Read `config/gate/accounts/{name}: AccountConfig`. If missing or `change.after.is_none()` (deletion), return `vec![]`.
3. Run `validate_account`. On error: write `validation_status` and `test_status: Failed { reason: "validation failed" }`. Return.
4. Write `test_status: Testing { started_at_ms: now() }` synchronously (returned in the Vec).
5. Abort any prior task for this account (via `Mutex<HashMap<String, AbortHandle>>` held in `&self`).
6. `ctx.spawn(async move { /* call transport.test_connection; write Success/Failed via ctx.writer */ })`. Stash the `AbortHandle`.
7. Return the synchronous writes.

- [ ] **Step 1:** Tests, with `MockTransport` and `MockSpawn`:
    - `test_writes_validation_status_then_short_circuits_on_invalid_endpoint`.
    - `test_writes_testing_then_success_on_valid_response`.
    - `test_writes_testing_then_failed_on_transport_error`.
    - `test_supersession_aborts_prior_task` (write twice rapidly; assert MockSpawn's recorded prior AbortHandle is in the aborted state after the second trigger fires; assert only the second's Success/Failed status lands).
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(gate): AccountTestSubscription with supersession`.

### Task N4: `CatalogRefreshSubscription`

**Files:** create `crates/ox-gate/src/subscriptions/catalog_refresh.rs`.

Watches: `PathPattern::PrefixSuffix { prefix: oxpath!("config","gate","accounts"), suffix: oxpath!("refresh_now") }`.

`handle`:
1. Extract name; read AccountConfig; validate (same pattern).
2. Write `refresh_status: Refreshing`. Abort prior; spawn task.
3. Spawned task: call `transport.fetch_catalog`. Wrap each response item into `ModelInfo`. For each model with `max_*_tokens == None`, fall back to `known_family_metadata(id, dialect)`; set `source` accordingly (`Server` if codec gave tokens, `KnownTable` if fallback used).
4. Diff the new `Vec<ModelInfo>` against the existing `config/gate/accounts/{name}/models` to count `models_added` vs `models_updated`.
5. Write the new `Vec<ModelInfo>` and `refresh_status: Success { models_added, models_updated, completed_at_ms }`.
6. On failure: write `refresh_status: Failed { reason }`. Do **not** clobber the existing `models` record.

- [ ] **Step 1:** Tests:
    - `refresh_writes_models_to_account_path`.
    - `refresh_status_progresses`.
    - `refresh_failure_does_not_clobber_existing_catalog`.
    - `refresh_supersession`.
    - `refresh_fills_known_table_tokens_for_anthropic`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(gate): CatalogRefreshSubscription`.

### Task N5: `AccountDeleteSubscription`

**Files:** create `crates/ox-gate/src/subscriptions/account_delete.rs`.

Watches: `PathPattern::PrefixSuffix { prefix: oxpath!("config","gate","accounts"), suffix: oxpath!("delete_now") }`.

`handle` (synchronous; no async work):
1. Extract `name` from change path. If account missing in snapshot, return `vec![]`.
2. Build all writes in one `Vec<Write>`:
    - Delete `config/gate/accounts/{name}` (tombstone or `None` per the codebase's deletion semantics).
    - Delete `secret/keys/{name}` — keys live in the namespace (Task A0), so this is just another typed delete.
    - Delete the synthesized provider entry (`config/gate/providers/{name}` if it follows that scheme; check existing delete logic at `crates/ox-cli/src/settings_shell.rs:599-684`).
    - If `ui/settings/accounts/selected == Some(name)`: write `ui/settings/accounts/selected ← None`.
    - Write `ui/settings/cursor ← oxpath!("settings","accounts")`.

The handler is fully self-contained — no fs-side helper, no out-of-namespace side effects.

- [ ] **Step 1:** Tests: account record removed; key record removed; provider entry removed; selection cleared only when matching; cursor popped; no-op when account missing.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(gate): AccountDeleteSubscription as a single Vec<Write> orchestration`.

### Task N6: `AccountCreateSubscription`

**Files:** create `crates/ox-gate/src/subscriptions/account_create.rs`.

Watches: `Exact(config/gate/accounts/_create_now)`.

`handle`:
1. Read `change.after` as typed `CreateAccountRequest { name }`.
2. Validate `name` as `PathComponent::try_new`. On error: write banner via `ui/global/banner: GlobalBanner::Error { message, set_at_ms }`; return.
3. Build a default `AccountConfig` (provider defaults to `anthropic` for v1, user changes via Detail page).
4. Return writes:
    - `config/gate/accounts/{name}: AccountConfig` (default).
    - `ui/settings/accounts/selected: Some(name)`.
    - `ui/settings/cursor: settings/accounts/_detail`.

`CreateAccountRequest { name: String }` is defined in this module since it's the action's payload type. Add serde + a round-trip test.

- [ ] **Step 1:** Tests: writes default config + selection + cursor; rejects invalid name with banner.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(gate): AccountCreateSubscription`.

### Task N7: `ConfigSaveSubscription`

**Files:** create `crates/ox-gate/src/subscriptions/config_save.rs`.

Watches: `Exact(config/save)`.

`handle`: reads the live `config/*` subtree; serializes; writes to disk via the existing config-persistence helper. On failure: writes `ui/global/banner: Error { message }`.

If a save mechanism already exists (it likely does), the subscription is a thin wrapper — the protocol just gives it a uniform trigger.

- [ ] **Step 1:** Tests: success writes file; failure shows banner.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(gate): ConfigSaveSubscription`.

### Task N8: Subscription registration entry point

**Files:** create `crates/ox-gate/src/subscriptions/mod.rs`.

- [ ] **Step 1:** `pub fn register_all(broker: &mut Broker, transport: Arc<dyn Transport>)`. Constructs each subscription (passing `transport` where needed), wraps in `Arc`, calls `broker.register_subscription`.
- [ ] **Step 2:** Sanity test: register; write to `config/save`; assert the save subscription's effect.
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(gate): subscription registration entry point`.

---

## Phase O — Kernel resolution path

### Task O1: Update `read_model_config`

**Files:**
- Modify: `crates/ox-kernel/src/run.rs:683-699` (the existing function).
- Modify: tests in the same file that use `gate/defaults/*`.

- [ ] **Step 1:** Test first: `read_model_config_resolves_from_completion_role_and_catalog`. Set up a `LocalConfig` with `config/completions/primary: CompletionRole` and `config/gate/accounts/{account}/models: Vec<ModelInfo>`; assert `(model_id, max_tokens)` returned.
- [ ] **Step 2:** Implement: read `config/completions/primary` (return error if missing — "no primary completion role configured"), read `config/gate/accounts/{role.account}/models` (default to empty Vec), find the model by id, return `(role.model_id, max_output_tokens.unwrap_or(KERNEL_FALLBACK_MAX_TOKENS))`. `KERNEL_FALLBACK_MAX_TOKENS = 4096`.
- [ ] **Step 3:** Update legacy tests in `run.rs:1733`, `run.rs:2106`, etc. that set `gate/defaults/{model, max_tokens}`. Switch each to populate the new paths.
- [ ] **Step 4:** All kernel tests PASS.
- [ ] **Step 5:** Commit `refactor(kernel): resolve model from CompletionRole + per-account catalog`.

NOTE: the kernel returns `role.model_id` even when the catalog doesn't list it. This is fine — the catalog is informational; the dialect makes the request. Add a one-line comment noting the choice.

### Task O2: Remove `Defaults` from `GateStore`

**Files:** modify `crates/ox-gate/src/lib.rs`.

- [ ] **Step 1:** Delete the `Defaults` struct (lib.rs:25-41 per v0 plan).
- [ ] **Step 2:** Delete the `defaults` field on `GateStore` (lib.rs:55-61).
- [ ] **Step 3:** Remove every `defaults/*` arm in `GateStore::read` and `GateStore::write`.
- [ ] **Step 4:** `cargo test --workspace` PASS — any test that referenced `gate/defaults/*` should already be updated in O1.
- [ ] **Step 5:** Commit `refactor(gate): remove obsolete Defaults from GateStore`.

---

## Phase P — Wire dispatch and remove the bypass

### Task P1: Extend `dispatch::send_key` with cursor scoping

**Files:** modify `crates/ox-cli/src/dispatch.rs`.

The existing signature is something like:
```rust
pub async fn send_key(client, key_str, screen, flags) -> KeyDispatchOutcome
```

Add: `cursor: Option<Path>`, `&BindingRegistry`, `&CommandRegistry`, `&dyn Reader` (for command snapshot).

When `screen == Screen::Settings && cursor.is_some()`:
- Special-case `nav.ascend` (read cursor → registry.ascend → write).
- Otherwise: lookup binding via `BindingRegistry::lookup`; lookup command; call `command.run(snapshot)`; apply each Write via `client.write`.

For other screens, fall through to the existing typed-command-write path.

- [ ] **Step 1:** Update the function signature and callers.
- [ ] **Step 2:** Tests:
    - `settings_p_on_models_writes_completion_role` (full integration — broker + bindings + commands + write).
    - `settings_esc_on_index_emits_screen_exit_signal` (return value indicates exit).
    - `settings_unbound_key_returns_unhandled` (no writes; outcome `Unhandled`).
- [ ] **Step 3:** Tests PASS.
- [ ] **Step 4:** Commit `feat(cli): extend send_key with settings cursor + registries`.

### Task P2: Remove the bespoke settings bypass

**Files:** modify `crates/ox-cli/src/event_loop.rs`.

- [ ] **Step 1:** Delete the bypass branch at lines ~775-789:
    ```
    if let ScreenSnapshot::Settings(_) = &ui.screen {
        let inbox_root = ...;
        if let Outcome::Handled = crate::settings_shell::handle_key(...).await { return; }
    }
    ```
- [ ] **Step 2:** Replace `SettingsShell::new()` construction with:
    ```rust
    let mut settings_renderers = settings::registry::RendererRegistry::new();
    let mut settings_commands  = settings::command_registry::CommandRegistry::new();
    let mut settings_bindings  = settings::binding_registry::BindingRegistry::new();
    settings::renderers::register_all(&mut settings_renderers);
    settings::commands::register_all(&mut settings_commands);
    settings::bindings::register(&mut settings_bindings);
    ```
- [ ] **Step 3:** Replace the settings render call with the snapshot-fetch + render path:
    ```rust
    let snap = settings::snapshot::fetch_settings_view_state(&client).await;
    let cursor = snap.read_typed(&oxpath!("ui","settings","cursor"))
        .ok().flatten().unwrap_or_else(|| oxpath!("settings","index"));
    let render_ctx = RenderCtx { area, data: &snap, registry: &settings_renderers, theme: &theme };
    let view = settings_renderers.render(&cursor, &render_ctx);
    terminal.draw(|frame| view_render::render_to_frame(&view, frame, frame.area(), &theme))?;
    ```
- [ ] **Step 4:** Update the `dispatch_key` call site to pass cursor + registries + snapshot.
- [ ] **Step 5:** `cargo check -p ox-cli` clean.
- [ ] **Step 6:** `cargo test --workspace` PASS.
- [ ] **Step 7:** Commit `refactor(cli): route settings through registry; remove bespoke bypass`.

### Task P3: Delete obsolete settings code

**Files:**
- Delete: `crates/ox-cli/src/settings_state.rs`
- Delete: `crates/ox-cli/src/settings_shell.rs`
- Delete: `crates/ox-cli/src/settings_view.rs`
- Modify: `crates/ox-cli/src/lib.rs` (drop `mod` declarations)

- [ ] **Step 1:** Delete the files.
- [ ] **Step 2:** Remove `mod` declarations from `lib.rs`.
- [ ] **Step 3:** `cargo check --workspace` clean. Anything still referencing these modules will fail to compile and needs cleanup.
- [ ] **Step 4:** `cargo test --workspace` PASS.
- [ ] **Step 5:** Commit `chore(cli): delete obsolete settings_state, settings_shell, settings_view`.

### Task P4: Wire subscription registration at startup

**Files:** modify `crates/ox-cli/src/event_loop.rs`.

- [ ] **Step 1:** After broker construction, before the first frame:
    ```rust
    let transport = Arc::new(ox_gate::transport::HttpTransport::default());
    ox_gate::subscriptions::register_all(&mut broker, transport.clone());
    ```
- [ ] **Step 2:** `cargo build` clean.
- [ ] **Step 3:** Commit `feat(cli): register settings subscriptions at startup`.

---

## Phase Q — Index entries population + first-run

### Task Q1: Populate index entries

**Files:** create `crates/ox-cli/src/settings/bootstrap.rs`.

- [ ] **Step 1:** `pub async fn populate_index_entries(client: &ClientHandle) -> Result<()>` writes the day-one entries (Accounts and Models per spec §6.1) to `settings/index/entries/{id}`.
- [ ] **Step 2:** Call it once at startup in `event_loop::run_async`, after broker is up and subscriptions are registered.
- [ ] **Step 3:** Smoke test: empty config → settings opens with two index rows visible.
- [ ] **Step 4:** Commit `feat(cli): populate settings index entries at startup`.

### Task Q2: First-run cursor

**Files:** modify `crates/ox-cli/src/settings/snapshot.rs` (or a startup helper).

- [ ] **Step 1:** At startup, if `config/gate/accounts/*` is empty *and* `ui/settings/cursor` is absent, write `ui/settings/cursor ← oxpath!("settings","accounts","_new")`. This puts the user on the new-account overlay over an empty Accounts list — clear next step.
- [ ] **Step 2:** Smoke test: fresh config opens to `_new` overlay, not the index.
- [ ] **Step 3:** Commit `feat(cli): first-run lands on new-account overlay when accounts empty`.

### Task Q3: Legacy-config detected log

**Files:** modify the startup helper.

- [ ] **Step 1:** At startup, after the migration in A0 has run, check if any of these legacy paths exist on disk in the user's config dir but are *not* mirrored into the namespace: `gate/defaults/*`, `gate/providers/*/models`. If any exist, log once via `tracing::info!`: `"legacy settings detected at {paths}; the new schema is in use — see docs/superpowers/specs/2026-04-27-settings-screen-redesign.md §5.9 for what changed."` Five-line check; prevents "where did my config go?" support questions.
- [ ] **Step 2:** Smoke test against a config dir containing legacy paths: log appears; new code runs normally.
- [ ] **Step 3:** Commit `feat(cli): one-line log when legacy settings paths are detected`.

---

## Phase R — End-to-end integration tests

### Task R1: Headless harness

**Files:** create `crates/ox-cli/tests/settings_e2e.rs`.

A small harness that:

1. Spins up an in-process broker with `LocalConfig` mounts.
2. Registers settings subscriptions (with a `MockTransport` so no network calls).
3. Registers renderers/commands/bindings.
4. Drives keystrokes via the dispatch path.
5. After each step, asserts namespace state and (optionally) the rendered View structure.

Day-one scenarios:

- [ ] **`navigate_index_to_models_set_primary`**: index → j → Enter → P → Esc → Esc. Assert `config/completions/primary` is set; cursor returns to index.
- [ ] **`add_account_create_flow`**: empty list → a → type "anthropic-personal" → Enter. Assert `config/gate/accounts/anthropic-personal` exists with default config; cursor at `_detail`.
- [ ] **`delete_account_flow`**: pre-populate one account → cursor at accounts → d → y. Assert account record gone; selection cleared; cursor at `settings/accounts`.
- [ ] **`test_account_progresses_status`**: cursor at `_detail`, account valid → t. Wait for spawned task. Assert `test_status` transitioned `Idle → Testing → Success`.
- [ ] **`refresh_writes_catalog`**: cursor at models, account selected → r. Assert `config/gate/accounts/{account}/models` populated; `refresh_status: Success`.

- [ ] **Step 1:** Build the harness (~150 lines of plumbing + helper functions).
- [ ] **Step 2:** Implement each scenario.
- [ ] **Step 3:** `cargo test -p ox-cli --test settings_e2e` PASS.
- [ ] **Step 4:** Commit `test(cli): end-to-end settings flows`.

---

## Phase S — Manual smoke + cleanup

### Task S1: Manual smoke

- [ ] **Step 1:** `cargo build --release -p ox-cli`.
- [ ] **Step 2:** Run the binary against a fresh config dir.
- [ ] **Step 3:** Verify by hand:
    - First-run lands on the new-account overlay with an empty Accounts list visible behind.
    - Type a name + Enter → cursor lands on Detail; default fields populated.
    - Edit Endpoint, press `t` → status block shows "Testing…" then "Success" or "Failed".
    - `Esc` returns to Accounts list.
    - `Esc` again returns to Index.
    - Index shows live Accounts count badge.
    - `Enter` on Models → empty list (no catalog yet).
    - With a valid account, press `r` on Models → catalog populates.
    - `P` on a model row → primary set; Index Models row badge updates.
    - `Esc` from Index exits the screen.
- [ ] **Step 4:** Note any UX issues; file follow-up issues.
- [ ] **Step 5:** Commit any inline fixes.

### Task S2: Cleanup

- [ ] **Step 1:** `cargo clippy --workspace -- -D warnings` — fix any new lints.
- [ ] **Step 2:** `cargo fmt --workspace`.
- [ ] **Step 3:** Confirm no `todo!()` or `unimplemented!()` remain.
- [ ] **Step 4:** Confirm the deleted-file list (P3) is fully gone.
- [ ] **Step 5:** Commit `chore: clippy + fmt cleanup`.

### Task S3: (Optional) Orphan path cleanup utility

- [ ] **Step 1:** Decide whether to ship `ox settings clean` for orphaned `gate/defaults/*` and `gate/providers/*/models` paths. Spec says no migration owed; v1 ships without it. Document the position in the release notes.

---

## Self-review checklist

- **Spec coverage.**
    - § 3.1 three trees → Phase D (View) + G (renderers) + I (snapshot of data).
    - § 3.2 the loop → Phase P (event loop wiring).
    - § 3.3 subscription protocol → Phase F (built); Phase N (impls).
    - § 3.4 identifiers as values → invariant maintained throughout (selection pointers hold values, account names validated).
    - § 4.1 cursor → registry's `ascend`, dispatch's nav special-case.
    - § 4.2 selection pointers → renderers + commands read them.
    - § 4.3 renderers + View → Phase D + E + G + J + K.
    - § 4.4 commands as Rust → Phase H + L.
    - § 4.5 bindings → Phase M.
    - § 4.6 actions via subscriptions → Phase N.
    - § 5 data shapes → Phases A + B + C + D.
    - § 6 concrete pages → Phases J + K + M.
    - § 7 error handling → embedded throughout (renderers handle empty-state; subscriptions write failed status; protocol bounds cascades; banner via global path).
    - § 8 testing → embedded per task; Phase R for E2E.
    - § 9 why-not → captured as design rationale; not implemented but referenced.
    - § 10 implementation sketch → this whole plan.
    - § 11 out of scope → wizard, multi-client, user customization — none scheduled.

- **Sequencing.** Phase A is the prereq. B/C are independent and parallelizable. D depends on nothing (new crate). E depends on D. F depends on C. G depends on D + C. H depends on C. I depends on (broker exists; not a phase). J depends on G + D. K depends on J. L depends on H + (some commands need C/specific pointers). M depends on L. N depends on F + B. O is independent of D-N. P depends on G + H + L + M + I + N. Q depends on P. R depends on Q. S depends on R.

- **Type consistency.** `CompletionRole.model_id`, `ModelKey.model_id`, `ModelInfo.id` — consistent. `RenderCtx`/`Renderer`/`RendererRegistry`/`AscendRule` — consistent. `Command`/`CommandRegistry`/`BindingRegistry` — consistent. `Subscription`/`SubscriptionRegistry`/`PathPattern`/`PathChange` — consistent.

- **Placeholder scan.** No `todo!()` markers in shipped code. The dispatch ascend special-case (L2) is identified up-front; the FieldInsert dispatcher specialization (L3) is identified up-front. No "implement later" deferrals.

- **Forward-compat checks.** `BindingEntry` is a serde record (user customization v2). `View` is in-memory only but its enum is shape-compatible with future serialization (Rio v2). Subscription protocol is single-process v1 but the trait shape survives a future wire-protocol lift.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-27-settings-screen-redesign.md`.**

19 phases, ~50 tasks, six crates. **Recommended: hybrid execution.**

**Phases A–F (foundations) → subagent-driven.** These set the precedents — keys-into-StructFS, type relocation, typed records, the View enum, the View→ratatui translator, the subscription protocol. The codebase will tell you fast if a foundation choice is wrong; a fresh subagent per task with review between gives the best feedback loop. ~14 tasks, ~14 dispatches.

**Phases G–S (application + integration) → inline execution with checkpoints.** From the renderer registry onward, the work is structurally repetitive — once one renderer is right, the engineer can stamp out the rest with the pattern in working memory. ~36 tasks at one-subagent-each is expensive (50 dispatches × full context overhead). Inline execution lets you batch within a phase, checkpoint at phase boundaries, and review diffs in flight.

**Crossover signal:** if Phase G's renderer-registry task lands cleanly and the first renderer (J1: IndexRenderer) feels like pattern-following rather than design, switch to inline. If you're still negotiating shape at J3, stay subagent-driven longer.

Alternative: full subagent-driven if you want maximum parallelism and don't mind the cost. Avoid full inline — the foundations are subtle enough that fresh-context review catches things that mid-stream review misses.

Which approach?
