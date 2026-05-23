# Horns settings tests: render end-to-end through TestBackend

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the inline rendering path from behavior tests. The settings-screen behavior tests drive inputs through the broker and assert on the rendered terminal buffer of a `TestBackend` — the same `render_to_frame` call the production ratatui backend makes. There is one rendering pipeline. The user-visible output is the test surface.

**Architecture:** Generalize `horns_ratatui::install` over `Backend: ratatui::backend::Backend`. Today it takes `Arc<Mutex<DefaultTerminal>>` (concrete `Terminal<CrosstermBackend<Stdout>>`). Make it `Arc<Mutex<Terminal<B>>>` with `B: Backend + Send + 'static`. The type parameter is contained inside `install` — downstream of `broker.register_subscription(Arc<dyn Subscription>)`, B is erased. Production callers (`ox-cli/src/horns_loop.rs`) infer B from `DefaultTerminal` and need no source changes. Tests install with `TestBackend`, drive chords through the broker, and read terminal cells from the test backend's buffer.

The architectural claim being enforced: **behavior tests run the production pipeline end-to-end.** Input goes in via broker writes; output comes out via the rendered cell buffer. No intermediate convenience surfaces. The only escape is unit tests of pure renderer functions — those live in `src/.../tests` modules, never in `tests/`.

**Tech Stack:** Rust 2024, `ratatui` (TestBackend + Terminal + Backend trait), `horns-ratatui`, `horns-core` (subscription substrate, unchanged), `ox-broker` (unchanged), `ox-cli/tests`.

**Reference documents:**
- `docs/superpowers/specs/2026-05-22-horns-test-quality-debt.md` — the debt this plan eliminates (Problem 1).
- `crates/horns/docs/{ui_framework.md, architecture.md}` — the framework's separation of substrate vs instance.

---

## Where we are (context for cold-start executors)

The settings-screen tests in `crates/ox-cli/tests/horns_settings_ui.rs` already drive **dispatch** through the broker correctly (Task 6 of the structfs-read-at-prefix plan migrated `press_chord` to broker writes). What they don't do: render through the production pipeline. The current test helper `render_settings(&client)` constructs a `RendererRegistry` + `SettingsSnapshot` and calls `.render()` inline:

```rust
async fn render_settings(client: &ClientHandle) -> View {
    let mut renderers = settings::RendererRegistry::new();
    settings::renderers::register_all(&mut renderers);
    let mut snap = fetch_settings_view_state(client).await;
    // … inline render, returns View tree …
}
```

This shadows the production path. Production: `RenderSubscription` (in `crates/horns-core/src/install.rs`) serializes the View via `structfs_serde_store::to_value`, writes the Record to `render_output_path`. `ViewRenderSubscription` (in `crates/horns-ratatui/src/install.rs`) watches that path, decodes back to View, and calls `render_to_frame(view, frame, area, &theme)` inside `terminal.draw(...)`. Today this only happens with `DefaultTerminal` (crossterm + stdout). Tests can't easily install it.

**What this plan changes:**
- `horns_ratatui::install` becomes generic over Backend.
- Behavior tests install with `TestBackend`, read cells from the buffer, assert on what the user sees.
- The inline-render helper disappears. The View-tree probes (`collect_list_primaries`, `find_first_list_with_item_primary`, `walk_view`) disappear with it — they only existed to inspect the View tree, which tests no longer construct.

**Tests that should keep passing:**
- `crates/ox-cli/tests/horns_settings_render.rs` — the one existing end-to-end render test. Already uses the production pipeline. Should remain green throughout.
- `crates/ox-cli/tests/settings_e2e.rs` — already uses `TestBackend` with insta frame snapshots. Production-realistic; left alone.
- `crates/ox-cli/src/settings/renderers/index.rs::tests` and other `src/.../tests` modules — these test renderers/commands as pure functions, no broker. Stay as-is.

**Architectural principles to preserve:**

1. **`ViewRenderSubscription` is the only thing that draws.** No test renders inline. No test calls `Renderer::render` outside `src/.../tests` modules.
2. **No `RendererRegistry::new()` in `tests/horns_settings_ui.rs`.** After this plan, the import is gone too.
3. **The terminal lock semantics stay as today.** `Arc<Mutex<Terminal<B>>>` shared between the host (currently: `horns_loop`) and `ViewRenderSubscription`. In tests, the test owns one end of the lock to read the buffer between cascade settlings; the subscription owns the other end.

---

## Conventions used throughout this plan

- **Paths anchored at `/Users/alex/Devel/AdjectiveNoun/ox/`** unless noted.
- **Verification:** `./scripts/quality_gates.sh`. Ignore the two pre-existing environmental failures (`prettier --check (site)` network, `wasm-pack build` leftover) when present. Anything else red blocks.
- **Commit messages:** ≤72 char subject, lowercase prefix (`refactor:`, `test:`, `feat:`, `tweak:`), no Co-Authored-By, HEREDOC.
- **Comments:** WHY only; no WHAT/phase/PR metadata.
- **No worktrees** — edits land directly.

---

## Task 1: Generalize `horns_ratatui::install` over Backend

**Goal:** `install` takes any `Terminal<B>` where `B: Backend + Send + 'static`. `ViewRenderSubscription` is generic over the same B and stored as `Arc<dyn Subscription>` in the broker (so the type parameter is erased at the registration boundary).

**Files:**
- Modify: `crates/horns-ratatui/src/install.rs`

### Step 1: Read the current shape

`RatatuiOptions::terminal: Arc<Mutex<DefaultTerminal>>` and `ViewRenderSubscription::terminal: Arc<Mutex<DefaultTerminal>>` are concrete. `DefaultTerminal = Terminal<CrosstermBackend<Stdout>>` per ratatui's prelude. `install` is `pub fn install(broker: &BrokerStore, opts: RatatuiOptions) -> RatatuiHandle`. The handler calls `terminal.lock().draw(|frame| render_to_frame(...))` — `Terminal::draw` is generic over backend already, so the call site needs no changes.

### Step 2: Add the type parameter

Apply across the file:

```rust
use ratatui::backend::Backend;
use ratatui::Terminal;
use std::sync::Arc;
use parking_lot::Mutex;

pub struct RatatuiOptions<B: Backend + Send + 'static> {
    pub view_input_path: Path,
    pub terminal: Arc<Mutex<Terminal<B>>>,
    pub theme: Theme,
}

pub fn install<B>(broker: &BrokerStore, opts: RatatuiOptions<B>) -> RatatuiHandle
where
    B: Backend + Send + 'static,
{
    let sub = ViewRenderSubscription::<B> {
        id: SubscriptionId("horns_ratatui.view_render".to_string()),
        watches: vec![PathPattern::Exact(opts.view_input_path.clone())],
        view_input_path: opts.view_input_path,
        terminal: opts.terminal,
        theme: opts.theme,
    };
    let id = sub.id.clone();
    broker.register_subscription(Arc::new(sub));
    RatatuiHandle { subscription_id: id }
}

struct ViewRenderSubscription<B: Backend + Send + 'static> {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    view_input_path: Path,
    terminal: Arc<Mutex<Terminal<B>>>,
    theme: Theme,
}

impl<B: Backend + Send + 'static> Subscription for ViewRenderSubscription<B> {
    // bodies unchanged
}
```

`RatatuiHandle` does NOT need a type parameter — it only holds the subscription id.

### Step 3: Verify production callers infer B

`crates/ox-cli/src/horns_loop.rs` calls:
```rust
horns_ratatui::install(
    broker,
    horns_ratatui::RatatuiOptions {
        view_input_path: crate::settings::render_output_path(),
        terminal: terminal_arc.clone(),
        theme: horns_ratatui::Theme::default(),
    },
)
```

`terminal_arc: Arc<Mutex<DefaultTerminal>>` — B = `CrosstermBackend<Stdout>` is inferred. No source changes needed.

### Step 4: Build + existing tests

```
cargo build -p horns-ratatui -p ox-cli
cargo test -p horns-ratatui
cargo test -p ox-cli --test horns_settings_render
```

Everything passes. The render-pipeline test (`horns_settings_render.rs`) already exercises `install` end-to-end; if the generic compiles and the existing test passes, the API change is sound.

### Step 5: Commit

```
git add crates/horns-ratatui/src/install.rs
git commit -m "refactor: generalize horns_ratatui::install over Backend"
```

---

## Task 2: Test fixture — broker + ratatui with TestBackend

**Goal:** A single helper that builds the broker, installs settings + ratatui (with `TestBackend`), and returns both the client and a handle to the terminal so tests can read the buffer.

**Files:**
- Modify: `crates/ox-cli/tests/horns_settings_ui.rs`

### Step 1: Add the helper

Near the top of the test file, alongside `build_broker_with_seeds`:

```rust
use parking_lot::Mutex;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Build a broker with mounts + seeded index entries, install the
/// settings horns instance, and install `horns_ratatui` against a
/// `TestBackend`. Returns the broker, client, and the test terminal
/// — tests drive inputs via `client` and read rendered cells via the
/// terminal's `backend().buffer()`.
async fn build_horns_test_rig() -> (BrokerStore, ClientHandle, Arc<Mutex<Terminal<TestBackend>>>) {
    let broker = build_broker_with_seeds().await;
    let client = broker.client();
    settings::install(&broker).await.expect("settings::install");

    let backend = TestBackend::new(80, 24);
    let terminal = Arc::new(Mutex::new(Terminal::new(backend).expect("test terminal")));

    let _handle = horns_ratatui::install(
        &broker,
        horns_ratatui::RatatuiOptions {
            view_input_path: settings::render_output_path(),
            terminal: terminal.clone(),
            theme: horns_ratatui::Theme::default(),
        },
    );

    // Seed the area path so `RenderSubscription` has a `Rect` to render
    // against. Matches `seed_initial_state` in `horns_loop`.
    let area = horns_core::Rect::new(0, 0, 80, 24);
    client.write_typed(&settings::input_area_path(), &area).await.unwrap();

    (broker, client, terminal)
}
```

### Step 2: Add a cell-reading helper

```rust
/// Read every cell symbol from the test terminal's buffer, row-major,
/// joined with newlines. Use for substring assertions ("contains
/// 'alpha' somewhere") and for snapshot tests via insta.
fn rendered_text(terminal: &Arc<Mutex<Terminal<TestBackend>>>) -> String {
    let guard = terminal.lock();
    let buffer = guard.backend().buffer();
    let (w, h) = (buffer.area.width as usize, buffer.area.height as usize);
    let mut out = String::with_capacity((w + 1) * h);
    for y in 0..h {
        for x in 0..w {
            out.push_str(buffer[(x as u16, y as u16)].symbol());
        }
        out.push('\n');
    }
    out
}
```

The cell API names (`buffer.area`, `buffer[(x,y)].symbol()`) might differ slightly between ratatui versions; adapt to what compiles in this workspace's ratatui crate. The intent is unchanged.

### Step 3: Build + smoke

```
cargo build -p ox-cli --tests
```

No tests use the new helpers yet; just verify they compile.

### Step 4: Commit

```
git add crates/ox-cli/tests/horns_settings_ui.rs
git commit -m "test: add TestBackend-backed horns test rig"
```

---

## Task 3: Migrate one test as proof-of-concept

**Goal:** Pick the simplest behavior test, replace its inline-render assertion with a cell-read assertion against the TestBackend buffer. Verify the assertion catches what the inline-View version caught.

**Files:**
- Modify: `crates/ox-cli/tests/horns_settings_ui.rs`

### Step 1: Pick `initial_render_shows_accounts_and_models_section_headers`

This is the most straightforward: setup the rig, render once, assert "Accounts" and "Models" appear in the rendered output. Today it does:

```rust
let view = render_settings(&client).await;
let primaries = collect_list_primaries(&view);
assert!(primaries.iter().any(|p| p.contains("Accounts")));
assert!(primaries.iter().any(|p| p.contains("Models")));
```

### Step 2: Rewrite it

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_render_shows_accounts_and_models_section_headers() {
    let (_broker, client, terminal) = build_horns_test_rig().await;
    // Trigger the first render by seeding the focus cursor (the
    // RenderSubscription wakes on cursor changes).
    set_cursor(&client, &oxpath!("settings", "index")).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await; // cascade settle — Problem 2.

    let text = rendered_text(&terminal);
    assert!(text.contains("Accounts"), "Accounts header missing from rendered output:\n{text}");
    assert!(text.contains("Models"), "Models header missing from rendered output:\n{text}");
}
```

### Step 3: Verify the assertion is equivalent

Mutate `IndexRenderer` locally (e.g., comment out the section header emission) and confirm the test fails. Restore. The cell-read assertion must catch the same regression the View-tree assertion did.

### Step 4: Run

```
cargo test -p ox-cli --test horns_settings_ui initial_render_shows_accounts
```

Passes. Cell read returns non-empty text containing both headers.

### Step 5: Commit

```
git add crates/ox-cli/tests/horns_settings_ui.rs
git commit -m "test: render initial_render test through TestBackend"
```

---

## Task 4: Migrate the remaining behavior tests

**Goal:** Every behavior test in `horns_settings_ui.rs` uses `build_horns_test_rig` + cell assertions. The inline-render helpers (`render_settings`, `collect_list_primaries`, `find_first_list_with_item_primary`, `walk_view`) are deleted.

**Files:**
- Modify: `crates/ox-cli/tests/horns_settings_ui.rs`

### Step 1: Inventory the tests

```
grep -n "^async fn \|#\[tokio::test" crates/ox-cli/tests/horns_settings_ui.rs
```

There are ~10–12 behavior tests (post-Task-6 they all use `press_chord` and either `read_cursor` or `render_settings`). The `read_cursor`-based tests don't need migration — they assert on broker state, not on rendering, and the broker path is already production-realistic.

The tests that need rewriting are the ones that call `render_settings(&client)` and inspect the resulting View.

### Step 2: Per-test conversion

For each affected test:

- Replace `let broker = build_broker_with_seeds().await; let client = broker.client(); settings::install(&broker).await.expect(...);` with `let (_broker, client, terminal) = build_horns_test_rig().await;`.
- Replace `let view = render_settings(&client).await;` + View-tree probe assertions with `rendered_text(&terminal)` + substring/snapshot assertions.
- For tests asserting on **highlight state** (e.g., `highlighted_row_has_selected_flag_in_rendered_list`): the View-tree path checked `ListItem::selected_index`. The cell-read equivalent is checking the *rendered style* of the focused row — TestBackend's buffer preserves cell styles. Use `terminal.lock().backend().buffer()[(x,y)].style()` for targeted checks.

If a test's assertion is "the View has structure X" rather than "the user sees X," that test was over-coupled to the View representation. Convert it to a behavioral assertion ("after `j`, the row labeled 'alpha' is highlighted") or delete it.

### Step 3: Delete the inline-render scaffolding

After all tests are converted:

- Delete `fn render_settings(...)`.
- Delete `fn collect_list_primaries(...)`.
- Delete `fn find_first_list_with_item_primary(...)`.
- Delete `fn walk_view(...)`.
- Drop imports of `RendererRegistry`, `Renderer`, `View`, `ListItem` from the file.
- Drop `fetch_settings_view_state` import.

Run `cargo build -p ox-cli --tests` and let the compiler enumerate any leftover references.

### Step 4: Run all

```
./scripts/quality_gates.sh
```

Must be green (modulo environmental). The test gate runs every test; cell-based assertions must pass deterministically.

### Step 5: Commit

```
git add crates/ox-cli/tests/horns_settings_ui.rs
git commit -m "$(cat <<'CM'
test: behavior tests render through TestBackend end-to-end

Every behavior test in horns_settings_ui.rs now drives inputs through
the broker AND observes rendered cells from a TestBackend-backed
terminal — the same pipeline production runs. Deletes the inline
RendererRegistry construction, the SettingsSnapshot fetch, and the
View-tree probe helpers that existed only to inspect the bypassed
path.

The user-visible output is now the test surface. Changes to View tree
internals don't break tests; only changes to what the user actually
sees on the rendered terminal do.
CM
)"
```

---

## Task 5: Document the seam

**Goal:** Make the test architecture decision explicit so future contributors don't drift back to inline rendering.

**Files:**
- Add: a short note in `crates/ox-cli/tests/horns_settings_ui.rs`'s module-doc, OR
- Add: a one-paragraph section in `crates/horns/docs/howto.md` titled "Testing a horns instance"

### Step 1: Module-doc

At the top of `crates/ox-cli/tests/horns_settings_ui.rs`:

```rust
//! Behavior tests for the horns-driven settings screen.
//!
//! These tests run the production pipeline end-to-end:
//!
//! 1. Build a broker with the mounts settings::install writes to.
//! 2. Install settings AND horns_ratatui (against a TestBackend).
//! 3. Drive inputs by writing KeyChords to the broker's input path.
//! 4. Observe outputs by reading rendered terminal cells from the
//!    TestBackend's buffer.
//!
//! The user-visible output is the test surface. Tests assert on
//! rendered cells — what the user sees — not on intermediate View
//! tree structure. Renderer unit tests (which DO inspect View trees)
//! live in `crates/ox-cli/src/settings/renderers/index.rs::tests`,
//! never here.
//!
//! Do not construct a RendererRegistry in this file. Do not call a
//! Renderer's `render` method directly. If you need to inspect a
//! View tree, write a unit test in the renderer's module instead.
```

### Step 2: Commit

```
git add crates/ox-cli/tests/horns_settings_ui.rs
git commit -m "docs: pin behavior-test architecture in horns_settings_ui module doc"
```

---

## After this plan lands

What's true at the end:

1. The settings screen has one rendering path: `RenderSubscription` writes a serialized View; `ViewRenderSubscription` reads it and calls `render_to_frame`. Production and tests both run this path.
2. Behavior tests assert on rendered terminal cells via `TestBackend::buffer()`. The View tree is an internal representation; tests don't depend on its shape.
3. `RatatuiOptions` is generic over `Backend`. Hosts pick crossterm in production, TestBackend in tests. No special test-only render code in horns-ratatui.
4. The inline-render scaffolding is gone — `render_settings`, View-tree probes, snapshot construction in the test file. ~150 lines of test-only code deleted.

What's still future work:

- **Problem 2** (sleep(10ms) for cascade settling) is still present in `press_chord`. Solving it requires broker quiescence — separate plan. This plan keeps the sleep in place; the new tests inherit it.
- The architectural-enforcement question (lint? convention? type-system?) is left to convention + module doc. If drift becomes a problem, add a CI grep check in `scripts/quality_gates.sh`.
- `horns_settings_render.rs` (single existing end-to-end test) and `settings_e2e.rs` (insta frame snapshots) overlap with the new tests in shape but not in coverage. Worth considering whether they consolidate, but not in this plan.

## Self-review

- **Coverage:** Each task has files, steps, verification, commit. Task 5 is documentation because the architectural decision is the load-bearing piece.
- **Type-parameter risk:** `B: Backend + Send + 'static` is the bound used by `ratatui::Terminal` itself. If something needs `Sync` we'll find out at compile time; trivial to add.
- **Cell-read API risk:** TestBackend's buffer API name may differ in this workspace's ratatui version. Adapt the helper at write-time; semantics are stable.
- **Test parity risk:** Cell assertions are weaker than View-tree inspection for some properties (e.g., focus IDs aren't visible in cells). Convert by checking *style* (highlighted background, etc.) on the row in question. If a test really needs to know "the focus cursor is at path X," it should read the broker, not the rendered output — broker state IS the source of truth and tests can observe it directly.
- **Backward-compat:** None broken. Production callers infer the type parameter; tests opt in.
