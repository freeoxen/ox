# UI framework — how-to

Task-oriented recipes. Each one is self-contained: read just the
section you need.

If you've never read `architecture.md`, skim its 60-second pitch
first; otherwise come straight here.

## How-tos

- [Add a new screen / page](#add-a-new-screen--page)
- [Add a command](#add-a-command)
- [Add a binding](#add-a-binding)
- [Add a subscription](#add-a-subscription)
- [Read a typed value from the snapshot](#read-a-typed-value-from-the-snapshot)
- [Write a typed value through the broker][write-typed]
- [Encode/decode a Path as a Value](#encodedecode-a-path-as-a-value)

[write-typed]: #write-a-typed-value-through-the-broker
- [Encode/decode a Path as a Value](#encodedecode-a-path-as-a-value)
- [Test a renderer](#test-a-renderer)
- [Test a command](#test-a-command)
- [Test a subscription](#test-a-subscription)
- [Run a settings E2E test](#run-a-settings-e2e-test)

---

## Add a new screen / page

Worked example: an "Appearance" page with a theme selector.

### 1. Pick a cursor path

```rust
oxpath!("settings", "appearance")
```

### 2. Write the renderer

`crates/ox-cli/src/settings/renderers/appearance.rs`:

```rust
use ox_path::oxpath;
use ox_view::{ListItem, View};

use crate::settings::registry::{
    AscendRule, RenderCtx, Renderer, RendererRegistry,
};
use super::util::read_typed;

pub struct AppearanceRenderer;

impl Renderer for AppearanceRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let theme: Option<String> = read_typed(
            ctx.data,
            &oxpath!("config", "ui", "theme"),
        );
        let items = vec![
            ListItem {
                primary: "Dark".into(),
                secondary: None,
                badge: None,
            },
            ListItem {
                primary: "Light".into(),
                secondary: None,
                badge: None,
            },
        ];
        let selected = match theme.as_deref() {
            Some("dark")  => Some(0),
            Some("light") => Some(1),
            _             => None,
        };
        View::List {
            title: Some("Appearance".into()),
            items,
            selected,
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::NearestRegistered
    }
}

pub fn register(reg: &mut RendererRegistry) {
    reg.register(
        oxpath!("settings", "appearance"),
        Box::new(AppearanceRenderer),
    );
}
```

### 3. Wire it into the renderer registry

`crates/ox-cli/src/settings/renderers/mod.rs`:

```rust
pub mod appearance;
// ...
pub fn register_all(reg: &mut RendererRegistry) {
    // ... existing register calls ...
    appearance::register(reg);
}
```

### 4. Add the index entry (optional, for top-level pages)

`crates/ox-cli/src/settings/bootstrap.rs::populate_index_entries`:

```rust
let appearance_entry = SettingsIndexEntry {
    id: "appearance".to_string(),
    label: "Appearance".to_string(),
    description: "Theme + display preferences.".to_string(),
    target_cursor: oxpath!("settings", "appearance"),
    badge: BadgeSource::None,
};
client.write_typed(
    &oxpath!("settings", "index", "entries", "appearance"),
    &appearance_entry,
).await?;
```

### 5. Pick a selection pointer (if the page has rows)

```
ui/settings/appearance/selected: usize
```

### 6. Add commands + bindings (next two sections)

You'll likely want at minimum:

- `highlight.appearance.next` / `highlight.appearance.prev`
- `nav.descend.appearance`
- A binding for Enter, j/k

### 7. Test

```rust
// In renderers/appearance.rs `#[cfg(test)] mod tests`
#[test]
fn appearance_renders_two_themes() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("config", "ui", "theme"),
        Value::String("dark".into()),
    );
    let view = render(&mut snap);
    let expected = View::List {
        title: Some("Appearance".into()),
        items: vec![
            ListItem { primary: "Dark".into(),  secondary: None, badge: None },
            ListItem { primary: "Light".into(), secondary: None, badge: None },
        ],
        selected: Some(0),
    };
    assert_eq!(view, expected);
}
```

---

## Add a command

Use the `command!` macro in `crates/ox-cli/src/settings/commands/`.
The macro generates the struct + `Command` impl + `id`/`display`/
`scope` boilerplate.

```rust
command! {
    struct_name: ToggleSomething,
    id: "appearance.toggle_something",
    title: "Toggle Something",
    description: "Flip the something flag.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "appearance")),
    run: |snap, _ctx| {
        let current: bool = read_typed(
            snap,
            &oxpath!("config", "ui", "something"),
        ).unwrap_or(false);
        vec![Write {
            path: oxpath!("config", "ui", "something"),
            record: Record::parsed(Value::Bool(!current)),
        }]
    },
}
```

Register in the appropriate `register(reg)` fn under
`commands/`:

```rust
pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(ToggleSomething::new()));
    // ...
}
```

### Inert behaviour

When a command's preconditions aren't met (no selection, missing
data), return `vec![]`. The dispatcher treats empty writes as
"command ran but nothing happened" and falls through to the legacy
input-store path so global handlers still see the key.

```rust
run: |snap, _ctx| {
    let Some(name) = read_typed::<String>(
        snap,
        &oxpath!("ui", "settings", "accounts", "selected"),
    ) else {
        return vec![];  // inert
    };
    // ... do the thing ...
}
```

### Reading the just-pressed key

`ctx.last_keystroke: Option<KeyChord>` carries the chord that
triggered this dispatch. Used by `field.insert` (to pick which char
to insert) and `field.delete_back`.

```rust
run: |snap, ctx| {
    let Some(chord) = &ctx.last_keystroke else { return vec![]; };
    let KeyCodeRepr::Char(c) = chord.code else { return vec![]; };
    // ... insert c into the focused field ...
}
```

---

## Add a binding

`crates/ox-cli/src/settings/bindings.rs::register`. Each entry is a
literal `BindingEntry { ... }`:

```rust
reg.register(BindingEntry {
    screen:      Screen::Settings,
    cursor_path: Some(oxpath!("settings", "appearance")),
    mode:        None,
    key:         KeyChord {
        modifiers: KeyModifierSet::default(),
        code:      KeyCodeRepr::Char('j'),
    },
    command_id:  CommandId(String::from("highlight.appearance.next")),
});
```

For uppercase letters, set `shift: true`:

```rust
key: KeyChord {
    modifiers: KeyModifierSet { shift: true, ..Default::default() },
    code:      KeyCodeRepr::Char('P'),
},
```

For Ctrl+letter:

```rust
key: KeyChord {
    modifiers: KeyModifierSet { ctrl: true, ..Default::default() },
    code:      KeyCodeRepr::Char('s'),
},
```

For Shift+Tab, use `BackTab` with `shift: true` (not `Tab` with
`shift: true` — `BackTab` is the crossterm-native code; the
`parse_key_str` helper translates the wire string).

### Specificity

The registry sorts bindings by specificity at insertion. Most-specific
to least:

1. `cursor_path: Some + mode: Some`
2. `cursor_path: Some + mode: None`
3. `cursor_path: None + mode: Some`
4. `cursor_path: None + mode: None`

Within a class, registration order breaks ties.

### Text-editing scopes

If your screen has editable text fields, register the printable-char
helper:

```rust
register_text_editing(reg, oxpath!("settings", "appearance", "_edit"));
```

That registers ~96 entries — printable ASCII + Backspace — at the
given cursor scope, all routed to `field.insert` /
`field.delete_back`. Specific bindings (e.g. `t` → `account.test`)
should be registered *before* the helper so registration order
breaks ties in favour of the specific binding.

---

## Add a subscription

For an action like "export config":

### 1. Pick a watched path

`Exact(oxpath!("config","_export_now"))` for collection-level.
`PrefixSuffix { prefix, suffix: oxpath!("export_now") }` for
per-instance.

### 2. Write a command that triggers it

```rust
command! {
    struct_name: ConfigExport,
    id: "config.export",
    title: "Export Config",
    description: "Trigger the export subscription.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("config", "_export_now"),
        record: Record::parsed(Value::Null),
    }],
}
```

### 3. Write the subscription

`crates/ox-gate/src/subscriptions/config_export.rs`:

```rust
use std::sync::Arc;

use ox_broker::subscription::{Subscription, SubCtx};
use ox_path::oxpath;
use ox_types::subscription::{PathPattern, SubscriptionId, Write};

pub struct ConfigExportSubscription {
    id:      SubscriptionId,
    watches: Vec<PathPattern>,
}

impl ConfigExportSubscription {
    pub fn new() -> Self {
        Self {
            id:      SubscriptionId(String::from("gate.config_export")),
            watches: vec![PathPattern::Exact(
                oxpath!("config", "_export_now"),
            )],
        }
    }
}

impl Default for ConfigExportSubscription {
    fn default() -> Self { Self::new() }
}

impl Subscription for ConfigExportSubscription {
    fn id(&self)      -> &SubscriptionId { &self.id }
    fn watches(&self) -> &[PathPattern]  { &self.watches }
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        // Read whatever from ctx.snapshot.
        // Spawn async work via ctx.spawn(...) if needed.
        // Return Vec<Write> for synchronous status updates.
        vec![]
    }
}
```

### 4. Register it

`crates/ox-gate/src/subscriptions/mod.rs::register_all`:

```rust
broker.register_subscription(Arc::new(
    config_export::ConfigExportSubscription::new(),
));
```

### 5. Spawning async work

```rust
fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
    let writer    = ctx.writer.clone();
    let transport = self.transport.clone();
    let synchronous_writes = vec![
        Write {
            path: status_path,
            record: Record::parsed(/* Refreshing */),
        },
    ];
    ctx.spawn.spawn(Box::pin(async move {
        let outcome = transport.fetch_catalog(...).await;
        // ... build the result write ...
        let _ = writer.write(path, record).await;
    }));
    synchronous_writes
}
```

### 6. Supersession

Hold a `Mutex<HashMap<String, AbortHandle>>` in `&self`. Before
spawning, abort the prior task for the same key:

```rust
in_flight: std::sync::Mutex<
    std::collections::HashMap<String, tokio::task::AbortHandle>
>,

// in handle:
if let Some(prior) = self.in_flight.lock().unwrap().remove(&name) {
    prior.abort();
}
let handle = ctx.spawn.spawn(Box::pin(async move { ... }));
self.in_flight.lock().unwrap().insert(name, handle);
```

---

## Read a typed value from the snapshot

Use `read_typed` from
`crates/ox-cli/src/settings/renderers/util.rs`:

```rust
use crate::settings::renderers::util::read_typed;

let role: Option<CompletionRole> = read_typed(
    ctx.data,
    &oxpath!("config", "gate", "completions", "primary"),
);
```

`read_typed` returns `None` when the path is missing OR when
deserialization fails (with a `tracing::warn!`). Renderers must be
total over Reader state — never panic.

For untyped reads inside a subscription handler, use the
`read_typed_via_reader` helper in
`crates/ox-gate/src/subscriptions/util.rs`.

For listing children under a prefix:

```rust
use crate::settings::renderers::util::child_names_under;

let names: Vec<String> = child_names_under(
    ctx.data,
    "config/gate/accounts",
);
```

For counting children:

```rust
use crate::settings::renderers::util::subtree_count;

let count: usize = subtree_count(ctx.data, "config/gate/accounts");
```

---

## Write a typed value through the broker

```rust
client.write_typed(&path, &typed_value).await?;
```

For `Path` values (which don't implement `Serialize`), use the
`path_to_value` helper from
`crate::settings::commands::navigation`:

```rust
use crate::settings::commands::navigation::path_to_value;

client.write(
    &oxpath!("ui", "settings", "cursor"),
    Record::parsed(path_to_value(&oxpath!("settings", "appearance"))),
).await?;
```

For `Null` writes (delete sentinel, or to fire a subscription):

```rust
client.write(
    &oxpath!("config", "save"),
    Record::parsed(Value::Null),
).await?;
```

---

## Encode/decode a Path as a Value

`Path` doesn't implement `Serialize`, so we encode it as a
`Value::Array<String>` of components.

```rust
use crate::settings::commands::navigation::{
    path_to_value, path_from_value,
};

let v = path_to_value(&oxpath!("settings", "accounts"));
// v = Value::Array([Value::String("settings"), Value::String("accounts")])

let p: Option<Path> = path_from_value(&v);
// p = Some(oxpath!("settings", "accounts"))
```

Used by:

- `ui/settings/cursor` writes (NavAscend, NavDescend*).
- `SettingsIndexEntry.target_cursor` (via `path_serde` adapter on
  serde-derived types).

The `path_serde` adapter at `crates/ox-types/src/path_serde.rs`
handles serde-context Paths automatically — annotate fields with:

```rust
#[serde(with = "crate::path_serde")]
pub target_cursor: Path,

// or for Option<Path>:
#[serde(with = "crate::path_serde::option")]
pub cursor_path: Option<Path>,
```

---

## Test a renderer

Build a `SettingsSnapshot` fixture, render, `assert_eq!` against the
expected View.

```rust
use ox_path::oxpath;
use ox_view::{ListItem, View};
use ratatui::layout::Rect;
use structfs_serde_store::to_value;

use crate::settings::registry::{RenderCtx, RendererRegistry};
use crate::settings::snapshot::SettingsSnapshot;
use crate::theme::Theme;

fn render(snap: &mut SettingsSnapshot) -> View {
    let theme = Theme::default();
    let registry = RendererRegistry::new();
    let mut ctx = RenderCtx {
        area: Rect::new(0, 0, 80, 24),
        data: snap,
        registry: &registry,
        theme: &theme,
    };
    AppearanceRenderer.render(&mut ctx)
}

#[test]
fn appearance_with_dark_theme() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("config", "ui", "theme"),
        Value::String("dark".into()),
    );
    let view = render(&mut snap);
    let expected = View::List {
        title: Some("Appearance".into()),
        items: vec![/* ... */],
        selected: Some(0),
    };
    assert_eq!(view, expected);
}
```

For typed entries, use `to_value(&typed)`:

```rust
let role = CompletionRole {
    account:  "anthropic".into(),
    model_id: "claude-sonnet-4-20250514".into(),
};
snap.insert(
    &oxpath!("config", "gate", "completions", "primary"),
    to_value(&role).unwrap(),
);
```

---

## Test a command

```rust
use crate::settings::command_registry::{Command, CommandCtx};
use crate::settings::registry::RendererRegistry;
use crate::settings::snapshot::SettingsSnapshot;

fn run<C: Command>(cmd: &C, snap: &mut SettingsSnapshot) -> Vec<Write> {
    let registry = RendererRegistry::new();
    let ctx = CommandCtx {
        registry: &registry,
        last_keystroke: None,
    };
    cmd.run(snap, &ctx)
}

#[test]
fn toggle_flips_the_flag() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("config", "ui", "something"),
        Value::Bool(false),
    );
    let writes = run(&ToggleSomething::new(), &mut snap);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("config", "ui", "something"));
    match &writes[0].record {
        Record::Parsed(Value::Bool(b)) => assert!(b),
        other => panic!("unexpected: {other:?}"),
    }
}
```

For commands that read `ctx.last_keystroke`, build a `KeyChord` and
pass it:

```rust
let ctx = CommandCtx {
    registry: &registry,
    last_keystroke: Some(KeyChord {
        modifiers: KeyModifierSet::default(),
        code:      KeyCodeRepr::Char('z'),
    }),
};
```

---

## Test a subscription

Use `MockTransport` and `TestSpawn` from
`crates/ox-gate/src/subscriptions/util.rs` (`#[cfg(test)]`-gated).

```rust
use std::sync::Arc;

use crate::subscriptions::util::testing::{
    CapturingWriter, InMemoryReader, MockTransport, TestSpawn,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_writes_models() {
    let transport = Arc::new(MockTransport::new()
        .with_catalog(Ok(vec![/* one ModelInfo */])));
    let sub = CatalogRefreshSubscription::new(transport.clone());

    let writer = Arc::new(CapturingWriter::new());
    let spawner = TestSpawn::new();
    let mut reader = InMemoryReader::new();
    // Pre-populate AccountConfig, ProviderConfig, ApiKey paths...

    let writes = sub.handle(SubCtx {
        snapshot: &mut reader,
        change:   /* a synthesized PathChange to test_now */,
        spawn:    &spawner,
        writer:   writer.clone(),
    });

    // Synchronous writes (`Refreshing` status).
    assert_eq!(writes.len(), 1);

    // Drive the spawned future to completion.
    spawner.run_all().await;

    // Now the writer has the Success + models writes.
    let captured = writer.captured();
    // ... assert the right writes landed ...
}
```

For supersession tests, call `handle` twice with the same trigger
path; assert the prior `AbortHandle::is_aborted()` after the second
call.

---

## Run a settings E2E test

`crates/ox-cli/tests/settings_e2e.rs` wires the whole stack — broker
+ subscriptions + renderers + commands + bindings — and drives
keystrokes via `dispatch::send_key`.

The harness:

```rust
let h = E2eHarness::new().await;
h.dispatch("j").await;             // highlight next
h.dispatch("Enter").await;         // descend
h.dispatch("P").await;              // set primary
h.dispatch("Esc").await;           // ascend
let cursor = h.current_cursor().await;
let primary: CompletionRole = h.client
    .read_typed(&oxpath!("config", "gate", "completions", "primary"))
    .await.expect("read").expect("present");
```

Async subscription work: poll the broker until the expected
post-state lands (or timeout fires). The `poll_until` helper does
40 × 50ms = 2s.

```rust
poll_until(|| async {
    h.client
        .read_typed::<AccountTestStatus>(&path)
        .await.ok().flatten()
        .map_or(false, |s| matches!(
            s, AccountTestStatus::Success { .. }
        ))
}).await;
```

Run with:

```
cargo test -p ox-cli --test settings_e2e
```
