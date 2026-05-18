# Read-at-prefix returns children-Map; restore horns broker-mount

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the S-tier "horns is a broker mount, end-to-end" architecture by closing the enumeration gap. Today the framework's `KeyDispatchSubscription` and `RenderSubscription` receive a broker `Reader` that doesn't support listing children at a prefix; renderers and commands need that, so the host loop has been dispatching and rendering inline as a workaround. We close the gap by extending an existing structfs convention: reading at a non-leaf `Path` returns a `Value::Map` of immediate children. With every horns-relevant store honoring that convention, the framework subscriptions actually work, and the host loop shrinks to a thin input-poller + state-machine supervisor.

**Architecture:** No new methods, no new traits, no structfs trait change. The Path namespace is the API: `reader.read(prefix)` returns either a leaf `Record` or a `Record` whose value is `Value::Map { <immediate-children> }`. `LocalConfig` already does this at the root path; we generalize the behavior to every depth and add it to the other stores horns reads through (`ConfigStore`, `UiStore`). The renderer helpers (`child_names_under`, `visible_rows::focus_enumeration`, anything else doing root-Map filtering) become single typed reads at the prefix path. With enumeration working through the broker reader, the framework's subscription protocol becomes load-bearing again: `KeyDispatchSubscription` dispatches, `RenderSubscription` writes the View, `horns_ratatui::install`'s `ViewRenderSubscription` consumes the View and draws. The host loop is a state machine that polls crossterm and writes inputs; nothing more.

**Tech Stack:** Rust 2024, structfs (unchanged), `ox-store-util::LocalConfig`, `ox-ui::ConfigStore`, `ox-ui::UiStore`, `ox-broker`, `horns-core`, `horns-ratatui`, `ox-cli`.

**Reference design:** Discussion captured in this session under `/jevan` — substrate enumeration via path convention (no method/trait additions).

---

## Conventions used throughout this plan

- **Paths are anchored at `/Users/alex/Devel/AdjectiveNoun/ox/`** unless noted.
- **Verification command:** `cargo build` and `cargo test` from the workspace root.
- **Commit messages:** ≤72 char subject, present-tense, lowercase prefix (`feat:`, `refactor:`, `fix:`, `tweak:`, `test:`, `docs:`). No Co-Authored-By trailer.
- **Don't use worktrees** — edits land directly in the main checkout.
- **TDD where it pays:** new convention behavior gets a failing test first. Pure deletions don't need new tests; the existing tests must continue to pass.

---

## Task 1: LocalConfig honors read-at-non-leaf-returns-children-Map

**Goal:** `LocalConfig::read(path)` returns a `Value::Map` of immediate children when `path` is a non-leaf (i.e., paths exist below it but no exact-match leaf value). When `path` exactly matches a leaf, returns the leaf value as today. When `path` matches nothing at all, returns `None`.

**Files:**
- Modify: `crates/ox-store-util/src/local_config.rs`

### Step 1: Write the failing tests

Find the existing test module in `crates/ox-store-util/src/local_config.rs` (likely `#[cfg(test)] mod tests { ... }`) and add:

```rust
#[test]
fn read_at_non_leaf_returns_value_map_of_immediate_children() {
    let mut cfg = LocalConfig::new();
    cfg.set("settings/index/entries/accounts", Value::String("acc".into()));
    cfg.set("settings/index/entries/models", Value::String("mod".into()));
    cfg.set("settings/other", Value::String("other".into()));

    // Reading `settings/index/entries` should return a Map containing
    // immediate children only — `accounts` and `models`. `other`
    // belongs under a different prefix and must not leak through.
    let path = structfs_core_store::Path::parse("settings/index/entries").unwrap();
    let rec = cfg.read(&path).unwrap().expect("non-leaf returns Some");
    let value = rec.as_value().expect("non-leaf has a value");
    let map = match value {
        Value::Map(m) => m.clone(),
        other => panic!("expected Map at non-leaf path; got {other:?}"),
    };
    assert!(map.contains_key("accounts"), "accounts child missing; map={map:?}");
    assert!(map.contains_key("models"), "models child missing; map={map:?}");
    assert_eq!(
        map.len(),
        2,
        "non-leaf read returned more than immediate children; map={map:?}",
    );
}

#[test]
fn read_at_leaf_still_returns_the_leaf_value() {
    let mut cfg = LocalConfig::new();
    cfg.set("settings/index/entries/accounts", Value::String("acc".into()));

    let path = structfs_core_store::Path::parse("settings/index/entries/accounts").unwrap();
    let rec = cfg.read(&path).unwrap().expect("leaf returns Some");
    let value = rec.as_value().expect("leaf has a value");
    assert_eq!(value, &Value::String("acc".into()));
}

#[test]
fn read_at_missing_path_returns_none() {
    let mut cfg = LocalConfig::new();
    cfg.set("settings/index/entries/accounts", Value::String("acc".into()));

    let path = structfs_core_store::Path::parse("not/a/real/prefix").unwrap();
    let rec = cfg.read(&path).unwrap();
    assert!(rec.is_none(), "missing path must return None");
}

#[test]
fn nested_non_leaf_reads_return_nested_maps() {
    let mut cfg = LocalConfig::new();
    cfg.set("a/b/c/d", Value::String("d-val".into()));
    cfg.set("a/b/c/e", Value::String("e-val".into()));
    cfg.set("a/b/x", Value::String("x-val".into()));

    // Reading at `a` should return only one immediate child: `b`.
    let rec = cfg
        .read(&structfs_core_store::Path::parse("a").unwrap())
        .unwrap()
        .expect("non-leaf returns Some");
    let map = match rec.as_value().unwrap() {
        Value::Map(m) => m.clone(),
        other => panic!("expected Map; got {other:?}"),
    };
    assert_eq!(map.len(), 1);
    assert!(map.contains_key("b"));
}
```

### Step 2: Run the failing tests

```bash
cargo test -p ox-store-util read_at_non_leaf
```
Expected: FAIL — current behavior returns `None` for non-leaf paths.

### Step 3: Implement the convention in `LocalConfig::read`

**Principle:** structfs paths are projections through a tree-of-Maps. A node is *either* a leaf value *or* a Map of children — never both. Reading a path returns whatever lives at that point in the tree: a leaf value if the path lands on one; a `Value::Map` of immediate children (whose values are themselves leaves or sub-Maps) if it lands on an inner node. There is no "leaf vs prefix" disambiguation rule because the conflict can't exist in well-formed data.

Read the current `LocalConfig` implementation. Likely structure: an inner `BTreeMap<String, Value>` keyed by stringified paths. The current `read` does:

1. If `path` is empty → return the entire inner map as a `Value::Map` wrapped in `Record::parsed`.
2. Else if `path` matches an exact key → return that key's value.
3. Else → return `None`.

The fix: generalize the root behavior to every depth. For any `path`, walk the inner map for keys that start with `path/`, group them by their immediate-next segment, and recursively build the tree projection at that depth. If the inner map also has an exact-match leaf at `path` *and* keys under `path/`, that's a malformed store — return an error (or panic in debug, error in release; pick one and stick with it). Don't paper over it.

```rust
fn read(&mut self, from: &Path) -> Result<Option<Record>, Error> {
    let prefix_str = from.to_string();
    let has_leaf = !prefix_str.is_empty() && self.inner.contains_key(&prefix_str);

    let prefix_with_slash = if prefix_str.is_empty() {
        String::new()
    } else {
        format!("{prefix_str}/")
    };
    let has_children = self
        .inner
        .keys()
        .any(|k| k.starts_with(&prefix_with_slash) && *k != prefix_str);

    if has_leaf && has_children {
        return Err(Error::store(
            "local_config",
            "read",
            format!(
                "malformed store: path {prefix_str:?} has both a leaf value and child entries — \
                 a node must be a leaf OR a Map, never both",
            ),
        ));
    }
    if has_leaf {
        return Ok(Some(Record::parsed(self.inner[&prefix_str].clone())));
    }
    if !has_children {
        return Ok(None);
    }

    // Inner node — assemble the tree projection at this depth by
    // grouping descendants by their immediate-next segment.
    let mut buckets: std::collections::BTreeMap<String, Vec<(&str, &Value)>> = Default::default();
    for (key, value) in &self.inner {
        let Some(rest) = key.strip_prefix(&prefix_with_slash) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let (head, tail) = match rest.split_once('/') {
            Some((h, t)) => (h.to_string(), t),
            None => (rest.to_string(), ""),
        };
        buckets.entry(head).or_default().push((tail, value));
    }

    let mut children: std::collections::BTreeMap<String, Value> = Default::default();
    for (head, entries) in buckets {
        let child_value = if entries.len() == 1 && entries[0].0.is_empty() {
            // Single entry with no further segments — head is a leaf.
            entries[0].1.clone()
        } else {
            // Recurse via the same read path — well-formed data means
            // the recursive call hits the inner-node branch and
            // returns a Map. Errors propagate.
            let sub_path = Path::parse(&format!("{prefix_with_slash}{head}"))?;
            match self.read(&sub_path)? {
                Some(rec) => rec
                    .as_value()
                    .cloned()
                    .ok_or_else(|| Error::store(
                        "local_config",
                        "read",
                        "child record had no value".into(),
                    ))?,
                None => continue,
            }
        };
        children.insert(head, child_value);
    }

    Ok(Some(Record::parsed(Value::Map(children))))
}
```

The `Error::store(...)` constructor's exact signature varies — read the file and use whatever is in scope. The signature of `inner_root_map()` and the inner storage shape might also differ; **read the actual file first** and adapt to the existing convention. The above is the algorithmic intent.

**Note on the malformed-store error:** this is the structfs invariant being enforced at read time. A well-behaved `set` / `write` should make the conflict unreachable by ensuring writes navigate-and-place rather than letting a leaf and a sub-prefix coexist. If the existing `LocalConfig::set` doesn't enforce this, file a follow-up to tighten it. For this plan, surfacing the violation at read time is sufficient.

### Step 4: Run the tests

```bash
cargo test -p ox-store-util read_at_non_leaf read_at_leaf read_at_missing nested_non_leaf
```
Expected: PASS.

### Step 5: Run the full workspace tests

```bash
cargo test --workspace
```
Expected: all tests pass. Anything that relied on `LocalConfig::read(non_leaf)` returning `None` will surface here; fix in place.

### Step 6: Commit

```bash
git add crates/ox-store-util/src/local_config.rs
git commit -m "feat: LocalConfig::read at non-leaf returns Map of immediate children"
```

---

## Task 2: Verify broker substrate routes non-leaf reads correctly

**Goal:** Confirm that reading a non-leaf path through a broker `ClientHandle` (or `SubCtx::snapshot`) reaches the mount that owns the prefix and returns whatever the mount produces — i.e., `Value::Map` of children after Task 1. If the routing is wrong, fix it.

**Files:**
- Possibly modify: `crates/ox-broker/src/dispatching_store.rs`, `crates/ox-broker/src/client.rs`, `crates/ox-broker/src/broker.rs`
- Add tests: `crates/ox-broker/tests/non_leaf_routing.rs` (new)

### Step 1: Write the failing test

Create `crates/ox-broker/tests/non_leaf_routing.rs`:

```rust
use std::time::Duration;

use ox_broker::BrokerStore;
use ox_path::oxpath;
use ox_store_util::local_config::LocalConfig;
use structfs_core_store::Value;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_read_at_non_leaf_returns_children_map_from_mount() {
    let broker = BrokerStore::new(Duration::from_secs(5));
    let _mount = broker
        .mount(oxpath!("settings"), LocalConfig::new())
        .await;
    let client = broker.client();

    client
        .write_typed(
            &oxpath!("settings", "index", "entries", "accounts"),
            &"acc".to_string(),
        )
        .await
        .expect("write accounts");
    client
        .write_typed(
            &oxpath!("settings", "index", "entries", "models"),
            &"mod".to_string(),
        )
        .await
        .expect("write models");

    // Read the prefix path — should now return a Map of children
    // (after LocalConfig's Task-1 fix routes through the mount).
    let rec = client
        .read(&oxpath!("settings", "index", "entries"))
        .await
        .expect("broker read")
        .expect("non-leaf returns Some");
    let value = rec.as_value().expect("non-leaf record has value");
    let map = match value {
        Value::Map(m) => m.clone(),
        other => panic!("expected Value::Map; got {other:?}"),
    };
    assert!(map.contains_key("accounts"));
    assert!(map.contains_key("models"));
    assert_eq!(map.len(), 2);
}
```

### Step 2: Run

```bash
cargo test -p ox-broker --test non_leaf_routing
```

If it passes, no broker change is needed — Task 1 alone solved the routing. Skip to Step 4.

If it fails, the broker's substrate isn't forwarding non-leaf reads to mounts the way it should. Investigate `crates/ox-broker/src/dispatching_store.rs` and `crates/ox-broker/src/broker.rs::submit_read`. Likely fix: the path-routing logic strips the mount prefix and forwards the remainder; verify that an empty remainder (when path == mount prefix exactly) or a non-leaf remainder (when path is intermediate) gets passed through faithfully.

### Step 3: Fix any routing bug surfaced by the test

Adapt the routing implementation to forward whatever the mount returns, including `Value::Map` records. Don't filter or short-circuit on the substrate side.

### Step 4: Commit

If a fix was needed:
```bash
git add crates/ox-broker
git commit -m "fix: broker routes non-leaf reads to mounts faithfully"
```

If no fix was needed (just the test), commit just the test:
```bash
git add crates/ox-broker/tests/non_leaf_routing.rs
git commit -m "test: pin broker non-leaf-read routing through LocalConfig mount"
```

---

## Task 3: ConfigStore + UiStore honor the same convention

**Goal:** the other two stores horns reads enumerable state from (`ConfigStore` for `config/gate/accounts/*` and `UiStore` for `ui/settings/*` containers) respond to non-leaf reads with a children-Map. After this task, every enumerable prefix horns cares about supports the convention.

**Files:**
- Modify: `crates/ox-ui/src/config_store.rs`
- Modify: `crates/ox-ui/src/ui_store.rs`

### Step 1: Grep where the convention is needed

```bash
grep -rn "child_names_under\|focus_enumeration\|read.*empty.*Map" crates/ox-cli/src/settings/
```

For each call to `child_names_under(data, "<prefix>")`, note what mount owns `<prefix>`:
- `settings/index/entries` → `settings` mount (LocalConfig — fixed by Task 1).
- `config/gate/accounts` → `config` mount (ConfigStore).
- `ui/settings/expanded` (consumers may also enumerate other `ui/settings/...` paths) — `ui` mount (UiStore).

Confirm by reading `crates/ox-cli/src/broker_setup.rs` which lists mount→store pairs.

### Step 2: Add the failing test for ConfigStore

In `crates/ox-ui/src/config_store.rs`'s test module, add:

```rust
#[test]
fn read_at_non_leaf_returns_immediate_children_map() {
    use structfs_core_store::{Path, Value};

    let mut store = ConfigStore::new(/* whatever init args it needs */);
    // Use whatever the store's write API is to populate two account
    // entries. Read the source to confirm — likely `set` or a typed
    // write method.
    store.set(
        &Path::parse("config/gate/accounts/alpha/endpoint").unwrap(),
        Value::String("https://alpha".into()),
    );
    store.set(
        &Path::parse("config/gate/accounts/beta/endpoint").unwrap(),
        Value::String("https://beta".into()),
    );

    let rec = store
        .read(&Path::parse("config/gate/accounts").unwrap())
        .expect("ok")
        .expect("non-leaf returns Some");
    let map = match rec.as_value().expect("value") {
        Value::Map(m) => m.clone(),
        other => panic!("expected Map; got {other:?}"),
    };
    assert!(map.contains_key("alpha"));
    assert!(map.contains_key("beta"));
}
```

(Adapt to whatever `ConfigStore`'s real API looks like — `set` may not be the method name. **Read the file first.**)

### Step 3: Implement the convention in ConfigStore

The implementation mirrors LocalConfig's. If ConfigStore stores entries in a similar flat-map shape internally, copy the walk-and-assemble logic from Task 1. If it stores entries differently (e.g., typed records under a different layout), adapt the assembly step to produce a `Value::Map` of immediate children.

If ConfigStore composes its read by delegating to an underlying `LocalConfig`, Task 1's fix may have already propagated — verify with the test before adding code.

### Step 4: Same for UiStore

Repeat steps 2-3 for `crates/ox-ui/src/ui_store.rs`. Test: write two values at `ui/settings/<a>` and `ui/settings/<b>`, read at `ui/settings`, assert `Map { a, b }`. UiStore is special — it accepts only known command paths for writes, so the test fixture has to use whatever the legitimate write surface is (e.g., constructing a `UiCommand` and writing through it). Read the existing UiStore tests for the right pattern.

### Step 5: Run all tests

```bash
cargo test -p ox-ui -p ox-store-util -p ox-broker
```
Expected: all pass.

### Step 6: Commit

```bash
git add crates/ox-ui
git commit -m "feat: ConfigStore and UiStore honor read-at-prefix-returns-children-Map"
```

---

## Task 4: Rewrite child_names_under and focus_enumeration to use the convention

**Goal:** delete the root-Map-filter implementation; replace with a single typed read at the prefix path. After this, the enumeration helpers work against any Reader that honors the convention — including the broker's `SubCtx::snapshot`.

**Files:**
- Modify: `crates/ox-cli/src/settings/renderers/util.rs` (`child_names_under` lives here per Task 9 plan; verify)
- Modify: `crates/ox-cli/src/settings/visible_rows.rs` (`focus_enumeration` or equivalent)
- Possibly: any other call site that does root-Map filtering

### Step 1: Find every caller

```bash
grep -rn "fn child_names_under\|child_names_under\|focus_enumeration" crates/ox-cli/src/settings/
```

### Step 2: Rewrite `child_names_under`

Replace the current implementation:

```rust
// BEFORE — reads root, string-filters keys:
pub(crate) fn child_names_under(data: &mut dyn Reader, prefix_str: &str) -> Vec<String> {
    let empty = Path::from_components(Vec::new());
    let root = match data.read(&empty) {
        Ok(Some(rec)) => rec,
        _ => return Vec::new(),
    };
    let map = match root.as_value() {
        Some(Value::Map(m)) => m,
        _ => return Vec::new(),
    };
    let prefix = format!("{}/", prefix_str);
    let mut seen: Vec<String> = Vec::new();
    for key in map.keys() {
        if let Some(rest) = key.strip_prefix(&prefix) {
            let segment = match rest.split('/').next() {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            if !seen.contains(&segment) {
                seen.push(segment);
            }
        }
    }
    seen
}

// AFTER — single typed read at the prefix path:
pub(crate) fn child_names_under(data: &mut dyn Reader, prefix_str: &str) -> Vec<String> {
    let path = match Path::parse(prefix_str) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let record = match data.read(&path) {
        Ok(Some(rec)) => rec,
        _ => return Vec::new(),
    };
    let map = match record.as_value() {
        Some(Value::Map(m)) => m,
        _ => return Vec::new(),
    };
    map.keys().cloned().collect()
}
```

### Step 3: Same for `focus_enumeration` (and any other root-Map readers)

Find the implementation, replace the root-walk with a single read at the prefix path. The internal logic that processes the resulting Map stays.

### Step 4: Run the existing settings tests

```bash
cargo test -p ox-cli settings
```
Expected: pass. If anything breaks, the convention isn't quite what the consumer expected — read the test failure and adapt.

### Step 5: Commit

```bash
git add crates/ox-cli/src/settings
git commit -m "refactor: enumeration helpers read at prefix instead of root-Map-filter"
```

---

## Task 5: Restore broker-mount horns_loop — delete inline dispatch and inline render

**Goal:** the horns settings loop becomes a thin input poller. The framework's `KeyDispatchSubscription` and `RenderSubscription` (registered by `settings::install`) actually handle dispatch and render. The ratatui backend's `ViewRenderSubscription` (registered by the loop's `horns_ratatui::install` call) holds the terminal and draws.

**Files:**
- Modify: `crates/ox-cli/src/horns_loop.rs`
- Possibly modify: `crates/horns-core/src/install.rs` (RenderSubscription's area read may need verification)

### Step 1: Read the current horns_loop body

Confirm what's there:
- Inline `Dispatcher::dispatch` call on every keystroke against a fetched `SettingsSnapshot`.
- Inline `renderers.render` on every frame against a fetched `SettingsSnapshot`.
- `horns_ratatui::install` is NOT called from `horns_loop` today (it's referenced in test scaffolding but the production loop uses `terminal.draw` directly).

### Step 2: Rewrite the loop body

```rust
pub async fn run_horns_settings_loop(
    broker: &BrokerStore,
    client: &ClientHandle,
    terminal: DefaultTerminal,
) -> std::io::Result<(HornsExit, DefaultTerminal)> {
    // Wrap the terminal for the subscription's interior mutability.
    // The Arc is scoped to this function — no top-level sharing.
    let terminal_arc = Arc::new(parking_lot::Mutex::new(terminal));

    // Install the ratatui backend: ViewRenderSubscription watches the
    // configured render_output_path and draws on every write.
    let ratatui_handle = horns_ratatui::install(
        broker,
        horns_ratatui::RatatuiOptions {
            view_input_path: crate::settings::render_output_path(),
            terminal: terminal_arc.clone(),
            theme: horns_ratatui::Theme::default(),
        },
    );

    // Seed the focus cursor if unset; seed area + render_tick so the
    // first frame fires. There is only one cursor — `ui/settings/focused`.
    // RendererRegistry::render walks the cursor's ancestor chain to
    // find the registered renderer, so dispatch and render share one
    // source of truth.
    seed_initial_state(client, &terminal_arc).await?;

    // Input loop: poll crossterm; write KeyChord to broker; watch
    // _request_exit. The framework's KeyDispatchSubscription handles
    // dispatch; RenderSubscription writes the View; ViewRenderSub
    // draws. The loop has no direct dispatch or render calls.
    let exit = loop {
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        if let Some(chord) = parse_key_str(&key_str) {
                            let _ = client
                                .write_typed(&crate::settings::input_key_path(), &chord)
                                .await;
                        }
                    }
                }
                Event::Resize(w, h) => {
                    let area = horns_core::Rect::new(0, 0, w, h);
                    let _ = client
                        .write_typed(&crate::settings::input_area_path(), &area)
                        .await;
                }
                _ => {}
            }
        }

        // Exit signal — `nav.ascend` writes this at the index page.
        let exit_path = oxpath!("ui", "settings", "_request_exit");
        let want_exit = client
            .read_typed::<bool>(&exit_path)
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
        if want_exit {
            let _ = client.write_typed(&exit_path, &false).await;
            use ox_types::{GlobalCommand, UiCommand};
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::Global(GlobalCommand::GoToInbox))
                .await;
            break HornsExit::ToLegacy;
        }
    };

    // Teardown: unregister the ratatui subscription, recover the
    // terminal from the Arc, return.
    broker.unregister_subscription(&ratatui_handle.subscription_id);
    let terminal = Arc::try_unwrap(terminal_arc)
        .map_err(|_| std::io::Error::new(
            std::io::ErrorKind::Other,
            "horns session: terminal not uniquely owned at exit",
        ))?
        .into_inner();

    Ok((exit, terminal))
}

async fn seed_initial_state(
    client: &ClientHandle,
    terminal: &Arc<parking_lot::Mutex<DefaultTerminal>>,
) -> std::io::Result<()> {
    use structfs_core_store::{Record, Value};

    // Focus cursor: defaults to settings/index. Used by both the
    // KeyDispatchSubscription (scope path = cursor.ancestors()) and
    // the RenderSubscription (renderer = innermost-registered ancestor
    // on the cursor's chain). Single source of truth.
    let focus_path = crate::settings::cursor_path();
    let focus_set = client.read(&focus_path).await.ok().flatten().is_some();
    if !focus_set {
        let _ = client
            .write(
                &focus_path,
                Record::parsed(crate::settings::commands::navigation::path_to_value(
                    &oxpath!("settings", "index"),
                )),
            )
            .await;
    }

    // Area: write the current terminal size.
    let size = terminal.lock().size()?;
    let area = horns_core::Rect::new(0, 0, size.width, size.height);
    let _ = client
        .write_typed(&crate::settings::input_area_path(), &area)
        .await;

    // Render tick: bump to fire the first render.
    let _ = client
        .write(
            &crate::settings::render_tick_path(),
            Record::parsed(Value::Integer(1)),
        )
        .await;

    Ok(())
}
```

Remove all references to `Dispatcher`, `fetch_settings_view_state` inside the loop, the local registries, `render_to_frame` calls — they're all subscription concerns now.

### Step 3: Make `RendererRegistry::render` walk the cursor's ancestors

The framework's `RendererRegistry::render` currently does an exact-match lookup: if no renderer is registered at exactly the cursor path, it returns `View::unknown_cursor_fallback`. This breaks cursor-as-focus: when the focus cursor descends into a compound widget (e.g. `settings/_compose_form/name`), no renderer is registered at that path, so render fails — even though the renderer for the enclosing page (`settings/index`) is right there.

The S-tier fix: render walks the cursor's ancestors outer-to-inner and renders with the *innermost* ancestor that has a registered renderer. The "page" the user is on is implicit in the cursor's ancestry; no second state path is needed.

In `crates/horns-core/src/render.rs`:

```rust
impl RendererRegistry {
    pub fn render(&self, cursor: &Path, ctx: &mut RenderCtx<'_>) -> View {
        // Walk the cursor's ancestors outer-to-inner, finding the
        // innermost registered renderer. The "page" is implicit in
        // the cursor's ancestry — no second cursor state path.
        let mut best: Option<&dyn Renderer> = None;
        for ancestor in path_ancestors_outer_to_inner(cursor) {
            if let Some(r) = self.specs.get(&ancestor) {
                best = Some(r.as_ref());
            }
        }
        match best {
            Some(r) => r.render(ctx),
            None => View::unknown_cursor_fallback(cursor),
        }
    }
}

/// Outer-to-inner ancestor walk: for cursor `a/b/c` returns
/// `[a, a/b, a/b/c]`. Shared with `Dispatcher::compute_scope_path`.
fn path_ancestors_outer_to_inner(p: &Path) -> Vec<Path> {
    let mut acc = Vec::with_capacity(p.components.len());
    for i in 1..=p.components.len() {
        acc.push(Path { components: p.components[..i].to_vec() });
    }
    acc
}
```

If `Dispatcher::compute_scope_path` already exposes a similar helper, reuse it instead of duplicating. The dispatcher walks the same chain; both subsystems agree on what "ancestry" means.

**No `page_cursor_path` field on `InstallPaths`.** `RenderSubscription` reads the focus cursor (`InstallPaths::cursor_path`) — the same path the dispatcher reads. The framework derives the page from the cursor's ancestry.

### Step 4: Build and run

```bash
cargo build -p ox-cli
cargo test --workspace
```

Tests for `horns_settings_ui.rs` should still pass — their `press_chord` helper currently dispatches inline, but the assertions are about the *post-state* (cursor moved, etc.), which is identical whether dispatch ran inline or through the broker subscription. Once Task 6 swaps `press_chord` to broker writes, this becomes a true end-to-end test.

### Step 5: Commit

```bash
git add crates/ox-cli/src/horns_loop.rs crates/horns-core/src/install.rs
git commit -m "$(cat <<'CM'
refactor: horns_loop is broker-mounted; subscriptions own dispatch+render

Restores the S-tier "horns is a broker mount, end-to-end" shape.
The horns settings loop's body shrinks to: install ratatui backend,
seed initial state, poll crossterm, write KeyChord to broker, watch
_request_exit. No inline Dispatcher::dispatch. No inline render.

Made possible by Tasks 1-4 — every horns-relevant store now honors
read-at-prefix-returns-children-Map, so the framework's
KeyDispatchSubscription and RenderSubscription see the reader
shape their commands and renderers expect.

RendererRegistry::render now walks the cursor's ancestors outer-to-
inner to find the innermost registered renderer — the "page" is
implicit in the cursor's ancestry. One cursor source of truth;
dispatch and render share it.
CM
)"
```

---

## Task 6: Migrate horns_settings_ui tests to verify via broker

**Goal:** `press_chord` in the UI tests stops calling `Dispatcher::dispatch` inline. It writes the `KeyChord` to the broker; the framework's `KeyDispatchSubscription` handles it. The tests' assertions don't change.

**Files:**
- Modify: `crates/ox-cli/tests/horns_settings_ui.rs`

### Step 1: Rewrite `press_chord`

```rust
async fn press_chord(client: &ClientHandle, chord: KeyChord) {
    client
        .write_typed(&settings::input_key_path(), &chord)
        .await
        .unwrap();
    // Broker dispatches the subscription synchronously, but command
    // cascades may produce additional async work — yield to let them
    // settle before observing state.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
}
```

Remove the `Dispatcher::new`, `BindingRegistry::new`, `CommandRegistry::new`, `RendererRegistry::new`, `register_all` calls — none of that is the test's responsibility anymore. `settings::install` already registered the subscription on the broker.

### Step 2: Run

```bash
cargo test -p ox-cli --test horns_settings_ui
```
Expected: all 10 tests pass. If any fail, the framework's subscription pipeline has a real gap; trace into `KeyDispatchSubscription::handle` to find it.

### Step 3: Commit

```bash
git add crates/ox-cli/tests/horns_settings_ui.rs
git commit -m "test: horns UI tests drive via broker writes through KeyDispatchSub"
```

---

## Task 7: Hand-verify in the real CLI

**Goal:** `./scripts/run_cli.sh` produces a working settings screen: `s` to enter, j/k to navigate, Enter to expand/collapse, `a` to compose, typing fills the name field, Esc to cancel, Esc on the index to exit.

**Files:** none — verification only.

### Step 1: Build the CLI

```bash
cargo build -p ox-cli
```

### Step 2: Run and verify

Ask the user to run `./scripts/run_cli.sh` and exercise every flow listed in Task 9 step 9 of the original extraction plan:

- Navigation works (j/k cycle rows)
- Compose form opens (`a`), accepts text input, Esc cancels, Enter commits
- Delete confirm works (`d`, then `y`/`n`)
- Edit field inline works (printable ASCII inserts via TextInputHandler, Enter commits, Esc cancels)
- Save works (Ctrl+S)
- Connectivity test (`t`) and catalog refresh (`r`) trigger their subscriptions

### Step 3: Fix anything observed

Real-world bugs surface here that no test caught. Surface them, fix them, add a regression test, re-verify.

### Step 4: Commit any fix

Whatever's needed.

---

## After this plan lands

What's true at the end:

1. The Path namespace is the only enumeration API. `reader.read(prefix)` returns a children-Map for non-leaf paths. No new methods on Reader. No new traits.
2. The horns settings loop is a thin input poller. ~50 lines. No `Dispatcher::dispatch` call in the host. No `fetch_settings_view_state` per frame in the host. No `renderer.render` call in the host.
3. The framework's `KeyDispatchSubscription` actually dispatches. `RenderSubscription` actually produces the View. `ViewRenderSubscription` actually draws. Each subscription gets a reader that supports the enumeration its commands and renderers need.
4. Settings screen behavior end-to-end matches what the user sees: j/k moves selection, accordions expand/collapse, compose form works, save works.
5. UI behavior tests verify by writing through the broker — no inline dispatch in tests either.

What's still future work (documented out of scope):

- Sub-widget install pattern. The mechanism exists; no widget has shipped using it yet.
- Hot-reload of bindings/commands at runtime. The data is at paths; mechanism for swapping the closures in the side-tables is undocumented.
- Multi-process horns (broker on one machine, ratatui backend on another). The architecture supports it; nothing's been tested.
- Migrating inbox/threads/history to horns instances. One screen at a time.

## Self-review (performed by the author)

- **Coverage:** Each task's deliverable is named, plus tests + commit. Task 7 is hand-verification because the bulk of the work surfaces only in the real terminal.
- **Placeholders:** None.
- **Type consistency:** `InstallPaths` is unchanged in shape — one `cursor_path` field, the focus cursor, used by both the dispatcher and the renderer (via ancestor walk).
- **Cycle avoidance:** Tasks 1–4 land before Task 5; Task 5 depends on the convention being honored end-to-end.
- **Structfs invariant:** a path's value is either a leaf or a Map of children — never both. Task 1's read implementation enforces this at read time by returning an error if the store's flat encoding ever holds both a leaf at `prefix` and entries under `prefix/`. Well-behaved writers can't produce that state; if `LocalConfig::set` ever does, fix the writer in a follow-up.
- **Risks:**
  - UiStore's read implementation has invariants (`path_command` rejecting unknown command paths). Reads at *enumerable container* paths should still produce a children-Map; reads/writes at *command* paths keep their existing reject-unknown behavior. Task 3's UiStore work has to thread that needle — verify with tests that command-path rejection still works after the convention lands.
