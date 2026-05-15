# UI framework — how-to

Task-oriented recipes. Each one is self-contained: read just the
section you need.

If you've never read `architecture.md`, skim its 60-second pitch
first; otherwise come straight here.

## How-tos

- [Pick a pattern: direct write vs. subscription](#pick-a-pattern-direct-write-vs-subscription)
- [Add a new screen / page](#add-a-new-screen--page)
- [Add a command](#add-a-command)
- [Add a binding](#add-a-binding)
- [Add an inline editing flow (a mode)](#add-an-inline-editing-flow-a-mode)
- [Add a confirmation flow](#add-a-confirmation-flow)
- [Add a subscription](#add-a-subscription)
- [Read a typed value from the snapshot](#read-a-typed-value-from-the-snapshot)
- [Write a typed value through the broker][write-typed]
- [Encode/decode a Path as a Value](#encodedecode-a-path-as-a-value)

[write-typed]: #write-a-typed-value-through-the-broker
- [Test a renderer](#test-a-renderer)
- [Test a command](#test-a-command)
- [Test a subscription](#test-a-subscription)
- [Run a settings E2E test](#run-a-settings-e2e-test)

---

## Pick a pattern: direct write vs. subscription

Before writing code, decide which architectural pattern fits the
work. Most "add a feature" tasks decompose into a command + a write
shape; getting the write shape right matters more than the command's
boilerplate.

### Decision matrix

| The work is… | Pattern |
|---|---|
| Mutating one or two paths synchronously | **Direct write from the command.** No subscription. |
| Mutating one path, with async or cross-cutting follow-up needed | **Direct write + reactive subscription** watching the data path. Subscription does the follow-up. |
| Async only — runs a network call, IO, or other non-instantaneous work the user requested | **Async-trigger subscription.** Command writes `Null` to a `…/<verb>_now` trigger path; subscription does the work. |
| "Open a sub-state on this page" (composing input, confirming an action, editing a field inline) | **Move the focus cursor.** Command writes `ui/settings/focused` into the widget's synthetic namespace (`settings/_compose_form/name`, `settings/_confirm_delete`, `settings/_edit`) and seeds the widget's working state (`ui/.../buffer`, `ui/.../cursor_saved`). Renderer + dispatcher react via cursor-ancestor walk. |
| "Navigate to a different page" | **Cursor write.** Command writes `ui/settings/cursor` to the new page path. |

### Anti-patterns to avoid

- **Sentinel-as-RPC.** Don't write `…/_create_now` and have a
  subscription read it, validate, and write the real path. The
  command should write the real path directly. Subscriptions are
  for follow-up, not translation.
- **Mode discriminator.** Don't add a `…/active: bool` or
  `…/stage: SomeEnum` flag at a UI-state path to tell the
  dispatcher which compound widget is engaged. Move the focus
  cursor (`ui/settings/focused`) into the widget's synthetic
  namespace (`settings/_my_widget/<leaf>`) and let the
  dispatcher's cursor-ancestor walk pick up the scope.
- **Synthetic display rows.** Don't push a `RowKind::FooAdd` into
  the visible-rows projection to show a "+ New X" affordance. The
  renderer reads UI-mode state and emits the affordance as a
  decoration. The projection contains only real things.

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

Subscriptions come in two shapes. Pick the one that matches the
work; if neither fits, the work probably doesn't earn a subscription
(see the decision matrix at the top of this document).

### Shape 1: Reactive observer

Use when a write to a real data-tree path needs async or
cross-cutting follow-up. The CLI does the data write directly; the
subscription watches the data path and fires the follow-up.

Worked example: when a new account appears, fetch its model catalog.

```rust
use ox_broker::subscription::{Subscription, SubCtx};
use ox_path::oxpath;
use ox_types::subscription::{PathPattern, SubscriptionId, Write};

pub struct CatalogFetchOnCreate {
    id:        SubscriptionId,
    watches:   Vec<PathPattern>,
    transport: Arc<dyn Transport>,
}

impl CatalogFetchOnCreate {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            id:        SubscriptionId("gate.catalog_fetch_on_create".into()),
            // Watch every account record under the collection.
            // The subscription handler filters: only fire when this
            // is a new entry (change.before.is_none()), not an
            // update.
            watches:   vec![PathPattern::Prefix(
                oxpath!("config", "gate", "accounts"),
            )],
            transport,
        }
    }
}

impl Subscription for CatalogFetchOnCreate {
    fn id(&self)      -> &SubscriptionId { &self.id }
    fn watches(&self) -> &[PathPattern]  { &self.watches }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        // Skip updates and deletes — only react to new entries.
        if ctx.change.before.is_some() || ctx.change.after.is_none() {
            return vec![];
        }
        // Skip nested writes (we want the account record itself,
        // not its children at .../models, .../test_status, etc.).
        let prefix = oxpath!("config", "gate", "accounts");
        if ctx.change.path.len() != prefix.len() + 1 {
            return vec![];
        }

        let account = ctx.change.path.components.last().cloned().unwrap();
        let writer = ctx.writer.clone();
        let transport = self.transport.clone();
        ctx.spawn.spawn(Box::pin(async move {
            let outcome = transport.fetch_catalog(&account, /* … */).await;
            // Write the catalog (or a Failed status) back.
            let _ = writer.write(/* path */, /* record */).await;
        }));
        vec![]  // No synchronous status writes for this example.
    }
}
```

The CLI does the create:

```rust
client.write_typed(
    &oxpath!("config", "gate", "accounts", &name),
    &AccountConfig::default(),
).await?;
```

The subscription fires off the catalog fetch in response. There is
no `_create_now` sentinel.

### Shape 2: Async action trigger

Use when the user requests an action that has *no* synchronous form
— a network call, file IO, anything where the work itself is
async. The trigger path uses the `…/<verb>_now` convention; the
command writes `Null` to the trigger; the subscription does the
work.

Worked example: connectivity test for an account.

```rust
command! {
    struct_name: AccountTest,
    id: "account.test",
    title: "Test Connection",
    description: "Test the account's API connectivity.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| {
        let Some(name) = read_typed::<String>(
            snap,
            &oxpath!("ui", "settings", "accounts", "selected"),
        ) else { return vec![]; };
        let comp = match PathComponent::try_new(&name) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        vec![Write {
            path: oxpath!("config", "gate", "accounts", comp, "test_now"),
            record: Record::parsed(Value::Null),
        }]
    },
}
```

```rust
impl Subscription for AccountTestSubscription {
    fn watches(&self) -> &[PathPattern] {
        // PrefixSuffix matches per-instance triggers without
        // hard-coding account names.
        &[PathPattern::PrefixSuffix {
            prefix: oxpath!("config", "gate", "accounts"),
            suffix: oxpath!("test_now"),
        }]
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        let name = instance_segment(
            &ctx.change.path,
            &oxpath!("config", "gate", "accounts"),
            &oxpath!("test_now"),
        ).expect("PrefixSuffix matched but couldn't extract segment");

        let writer = ctx.writer.clone();
        let transport = self.transport.clone();

        // Synchronous: write the in-progress status.
        let synchronous = vec![Write {
            path: oxpath!("config", "gate", "accounts", &name, "test_status"),
            record: Record::parsed(/* Testing { started_at_ms } */),
        }];

        ctx.spawn.spawn(Box::pin(async move {
            let outcome = transport.test_connection(&name, /* … */).await;
            // Write the result status.
            let _ = writer.write(/* path */, /* record */).await;
        }));

        synchronous
    }
}
```

The legitimacy of the `_now` trigger here is that the work is
fundamentally async — there's no synchronous version of "run a
network test." Without the trigger path, the CLI would have to
either (a) block on the network call (which it can't — commands are
sync) or (b) spawn its own task, fragmenting the async work across
the codebase.

### Register the subscription

`crates/ox-gate/src/subscriptions/mod.rs::register_all`:

```rust
broker.register_subscription(Arc::new(
    catalog_fetch_on_create::CatalogFetchOnCreate::new(transport.clone()),
));
broker.register_subscription(Arc::new(
    account_test::AccountTestSubscription::new(transport.clone()),
));
```

### Supersession

When a subscription spawns async work that may take a while, hold a
`Mutex<HashMap<String, AbortHandle>>` in `&self` and abort prior
in-flight tasks for the same key before spawning:

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

`AccountTestSubscription` and `CatalogRefreshSubscription` both use
this shape — see `crates/ox-gate/src/subscriptions/`.

---

## Add an inline editing flow (a compound widget)

Worked example: a "compose new connection" widget, triggered by
`a`, where the user types a name inline and presses Enter to
create.

The principle: a compound widget has a synthetic *cursor*
namespace (`settings/_compose_form/<leaf>`) and a sibling
*working-state* subtree (`ui/settings/new_account/{buffer, key,
protocol, errors, cursor_saved, ...}`). The cursor at one of the
synthetic leaves is the discriminator the dispatcher's
cursor-ancestor walk picks up.

### 1. Pick a synthetic cursor namespace and a working-state subtree

```
cursor (when widget engaged):  settings/_compose_form/{name,protocol,key,...}
working state subtree:         ui/settings/new_account/{buffer, key, protocol, errors, cursor_saved}
```

### 2. Open-widget command

```rust
command! {
    struct_name: AccountsAdd,
    id: "accounts.add",
    title: "Add Connection",
    description: "Open the inline name prompt.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| {
        // Save the current focus so cancel/commit can restore it.
        let saved = read_path(snap, &oxpath!("ui", "settings", "focused"))
            .unwrap_or_else(|| oxpath!("settings", "index"));
        vec![
            Write {
                path: oxpath!("ui", "settings", "new_account", "buffer"),
                record: Record::parsed(Value::String(String::new())),
            },
            Write {
                path: oxpath!("ui", "settings", "new_account", "cursor_saved"),
                record: Record::parsed(path_to_value(&saved)),
            },
            Write {
                path: oxpath!("ui", "settings", "focused"),
                record: Record::parsed(path_to_value(&oxpath!(
                    "settings", "_compose_form", "name"
                ))),
            },
        ]
    },
}
```

Bind it: `a` → `accounts.add` at `Phase::Bubble` on
`Exact(settings)` (page-level — fires whenever no inner scope
claims `a`).

### 3. Renderer reads the working state

The accounts-section renderer reads `new_account/buffer` plus the
cursor (for active-field decoration):

```rust
let buffer: Option<String> = read_typed(
    ctx.data,
    &oxpath!("ui", "settings", "new_account", "buffer"),
);
let header = match buffer {
    Some(b) => inline_name_prompt(&b),
    None    => static_create_affordance(),
};
```

The visible-rows projection does not change. There is no
`RowKind::AccountAdd`. The affordance is renderer-side only.

### 4. Register bindings under the widget's cursor scopes

The dispatcher's cursor-ancestor walk places these scopes on the
dispatch path automatically while the cursor is at
`settings/_compose_form/<leaf>`:

- `Exact(settings/_compose_form)` — `Esc` cancel,
  `Tab`/`Shift+Tab` advance at `Phase::Capture`; `Enter` commit at
  `Phase::Bubble`.
- `Exact(settings/_compose_form/name)` — printable ASCII →
  `accounts.compose.insert_char` at `Phase::Target`; Backspace →
  `accounts.compose.delete_back`.
- `Exact(settings/_compose_form/protocol)` — `h`/`l` →
  `accounts.compose.cycle_{back,fwd}` at `Phase::Target`.

No dispatcher edits. No discriminator reads. The right scopes ride
onto the dispatch path because the cursor's ancestors include
them.

### 5. Commit command writes the data + restores the cursor

```rust
fn commit_create(snap: &mut dyn Reader) -> Vec<Write> {
    let buffer: String = read_typed(
        snap,
        &oxpath!("ui", "settings", "new_account", "buffer"),
    ).unwrap_or_default();
    let trimmed = buffer.trim();
    if trimmed.is_empty() { return vec![]; }
    let name = match AccountName::try_new(trimmed) {
        Ok(n) => n,
        Err(reason) => return vec![banner_error(reason)],
    };
    vec![
        // The actual create — direct write, no sentinel.
        write_typed(
            &oxpath!("config", "gate", "accounts", name.as_path_component()),
            &AccountConfig::default(),
        ),
        // UI cascade — focus the new row, cascade-clear the compose
        // widget's working state subtree.
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&row_path_for(&name))),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account"),
            record: Record::parsed(Value::Null),
        },
    ]
}
```

Esc / cancel reads `ui/settings/new_account/cursor_saved`, writes
it back to `ui/settings/focused`, and cascade-clears the
`ui/settings/new_account` subtree — exiting the widget without
performing the create.

---

## Add a confirmation flow

Worked example: "delete this account — y/n confirmation."

Same cursor-as-focus pattern as compose / inline-edit: the cursor
moves into a synthetic widget namespace; the target and the
pre-open cursor live as working state under a sibling UI-state
path.

### 1. Pick a synthetic cursor path and a working-state subtree

```
cursor (when widget engaged):  settings/_confirm_delete
working state subtree:         ui/settings/pending_delete/{target_account, cursor_saved}
```

Cursor at `settings/_confirm_delete` = confirmation banner showing.
Cursor anywhere else = no confirmation.

### 2. Open-confirmation command

```rust
command! {
    struct_name: AccountsDeleteConfirm,
    id: "accounts.delete_confirm",
    title: "Delete Connection",
    description: "Show the delete confirmation banner.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| {
        let Some(name) = read_typed::<String>(
            snap,
            &oxpath!("ui", "settings", "accounts", "selected"),
        ) else { return vec![]; };
        let Some(saved) = read_path(snap, &oxpath!("ui", "settings", "focused"))
            else { return vec![]; };
        vec![
            Write {
                path: oxpath!("ui", "settings", "pending_delete", "target_account"),
                record: Record::parsed(Value::String(name)),
            },
            Write {
                path: oxpath!("ui", "settings", "pending_delete", "cursor_saved"),
                record: Record::parsed(path_to_value(&saved)),
            },
            Write {
                path: oxpath!("ui", "settings", "focused"),
                record: Record::parsed(path_to_value(&oxpath!("settings", "_confirm_delete"))),
            },
        ]
    },
}
```

### 3. Renderer reads the working state

```rust
let target: Option<String> = read_typed(
    ctx.data,
    &oxpath!("ui", "settings", "pending_delete", "target_account"),
);
if let Some(name) = target {
    // Render an inline confirmation banner above the accounts list:
    //   "Delete '<name>'? y / n"
}
```

### 4. Bindings under `Exact(settings/_confirm_delete)`

Bindings live on the widget's scope; the cursor's ancestor walk
puts the scope on the dispatch path automatically while the cursor
is at `settings/_confirm_delete`:

- `Esc` / `n` at `Phase::Capture` → `pending_delete.cancel`.
- `y` at `Phase::Target` (or Bubble) → `pending_delete.commit`.

No dispatcher mode-discriminator reads; no while-pending special
case in the dispatcher's command bodies.

### 5. Commit / cancel

```rust
fn commit_delete(snap: &mut dyn Reader) -> Vec<Write> {
    let target = read_typed::<String>(
        snap, &oxpath!("ui", "settings", "pending_delete", "target_account"),
    ).expect("widget invariant: set on open");
    let saved = read_path(
        snap, &oxpath!("ui", "settings", "pending_delete", "cursor_saved"),
    ).expect("widget invariant: set on open");
    let comp = PathComponent::try_new(&target).expect("validated on entry");
    vec![
        // The actual delete — Null write to the data path.
        Write {
            path: oxpath!("config", "gate", "accounts", comp),
            record: Record::parsed(Value::Null),
        },
        // Restore the pre-open cursor.
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&saved)),
        },
        // Cascade-clear the widget's working state subtree.
        Write {
            path: oxpath!("ui", "settings", "pending_delete"),
            record: Record::parsed(Value::Null),
        },
    ]
}

fn cancel_pending(snap: &mut dyn Reader) -> Vec<Write> {
    let saved = read_path(
        snap, &oxpath!("ui", "settings", "pending_delete", "cursor_saved"),
    ).expect("widget invariant: set on open");
    vec![
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&saved)),
        },
        Write {
            path: oxpath!("ui", "settings", "pending_delete"),
            record: Record::parsed(Value::Null),
        },
    ]
}
```

A subscription watching `Prefix(config/gate/accounts)` for null
writes can do the side-data cleanup (drop the API key, drop the
provider record if no other account uses it). That's reactive
follow-up; the delete itself is the CLI's direct write.

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
