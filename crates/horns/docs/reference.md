# horns — reference

Lookup-only. Type signatures, paths, the file map, and a glossary.
Code paths and module names refer to the current horns crate layout
(`crates/horns-core`, `crates/horns-ratatui`); the settings worked
example lives in `crates/ox-cli/src/settings/`.

## Public API surface

```rust
// horns-core:
pub use install::{install, build_install_bundle, build_install_bundle_from_registries,
                  HornsHandle, InstallOptions, InstallPaths, InstallBundle};
pub use binding::{BindingEntry, BindingId, BindingRegistry, BindingScope,
                  HandlerEntry, HandlerId, HandlerMetadata, KeyHandler, Phase};
pub use command::{Command, CommandCtx, CommandDisplay, CommandId, CommandMetadata,
                  CommandRegistry, CommandScope};
pub use key::{KeyChord, KeyCodeRepr, KeyModifierSet};
pub use render::{AscendRule, Renderer, RenderCtx, RendererMetadata, RendererRegistry, Rect};
pub use subscription::{PathChange, PathPattern, SubscriptionId,
                       Subscription, SubCtx, SpawnHandle, AsyncWriter, SubscriptionRegistry};
pub use view::View;
pub use write::Write;

// horns-ratatui:
pub use render::render_to_frame;
pub use theme::Theme;
```

## Types

### View

`crates/horns-core/src/view.rs`. No backend types; opt-in `serde`
support behind the `serde` feature.

```rust
pub enum View {
    Empty,
    Text  { spans: Vec<Span>, align: Align },
    Stack { dir: Direction, children: Vec<(View, Sizing)> },
    List  {
        title: Option<String>,
        items: Vec<ListItem>,
        selected: Option<usize>,
    },
    Form  {
        title: Option<String>,
        rows: Vec<FormRow>,
        focused: Option<usize>,
    },
    Modal {
        background: Box<View>,
        foreground: Box<View>,
        dim: bool,
    },
    Banner { kind: BannerKind, content: String },
    StatusBlock {
        title: String,
        lines: Vec<StyledLine>,
        scroll_offset: u16,
    },
    Pad { padding: Padding, child: Box<View> },
}
```

Supporting types:

```rust
pub struct ListItem {
    pub primary:   String,
    pub secondary: Option<String>,
    pub badge:     Option<String>,
}

pub struct FormRow {
    pub label: String,
    pub value: FormValue,
    pub error: Option<String>,
    pub hint:  Option<String>,
}

pub enum FormValue {
    Text { value: String, cursor: u32, masked: bool },
    Selector { options: Vec<String>, current: usize },
    ReadOnly(String),
}

pub enum BannerKind { Info, Error }
pub struct Span { pub text: String, pub style: Style }
pub struct StyledLine(pub Vec<Span>);
pub enum Direction { Horizontal, Vertical }
pub enum Sizing { Fill, Fixed(u16), Min(u16) }
pub struct Padding { pub top: u16, pub right: u16,
                     pub bottom: u16, pub left: u16 }
pub enum Align { Left, Center, Right }
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: ModifierSet,
}
pub enum Color {
    Reset, Black, Red, Green, Yellow, Blue, Magenta, Cyan,
    Gray, DarkGray,
    LightRed, LightGreen, LightYellow, LightBlue,
    LightMagenta, LightCyan, White,
    Indexed(u8),
    Rgb(u8, u8, u8),
}
pub struct ModifierSet {
    pub bold:      bool,
    pub italic:    bool,
    pub underline: bool,
    pub dim:       bool,
    pub reversed:  bool,
}
```

Convenience constructors:

```rust
View::text(s: impl Into<String>) -> View
View::stack_v(children: Vec<(View, Sizing)>) -> View
View::stack_h(children: Vec<(View, Sizing)>) -> View
View::pad(view: View, padding: Padding) -> View
View::unknown_cursor_fallback(cursor: &Path) -> View

Span::plain(s: impl Into<String>) -> Span
```

### Renderer

`crates/horns-core/src/render.rs`.

```rust
pub trait Renderer: Send + Sync {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View;
    fn ascend_to(&self) -> AscendRule;
}

pub struct RenderCtx<'a> {
    pub area:     Rect,
    pub data:     &'a mut dyn Reader,
    pub registry: &'a RendererRegistry,
    pub theme:    &'a dyn std::any::Any,  // backend downcasts
}

pub enum AscendRule {
    /// Walk the display-tree parent chain until a registered
    /// renderer matches.
    NearestRegistered,
    /// Ascend to the named cursor (typically the screen's index).
    /// Falls through to `_request_exit` if the target isn't
    /// registered.
    Fallback(Path),
    /// Top-level page; ascending exits the screen entirely.
    ExitScreen,
}

pub struct RendererMetadata {
    pub ascend_rule: AscendRule,
}

pub struct RendererRegistry { /* HashMap<Path, Box<dyn Renderer>> */ }

impl RendererRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, cursor: Path, r: Box<dyn Renderer>);
    pub fn lookup(&self, cursor: &Path) -> Option<&dyn Renderer>;
    pub fn render(
        &self,
        cursor: &Path,
        ctx: &mut RenderCtx<'_>,
    ) -> View;
    pub fn ascend(&self, cursor: &Path) -> Option<Path>;
}
```

### Command

`crates/horns-core/src/command.rs`.

```rust
pub trait Command: Send + Sync {
    fn id(&self)      -> &CommandId;
    fn display(&self) -> &CommandDisplay;
    fn scope(&self)   -> &CommandScope;
    fn run(
        &self,
        snapshot: &mut dyn Reader,
        ctx: &CommandCtx<'_>,
    ) -> Vec<Write>;
}

pub struct CommandCtx<'a> {
    pub registry:       &'a RendererRegistry,
    pub last_keystroke: Option<KeyChord>,
}

pub struct CommandRegistry { /* HashMap<CommandId, Box<dyn Command>> */ }

impl CommandRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, c: Box<dyn Command>);
    pub fn lookup(&self, id: &CommandId) -> Option<&dyn Command>;
    pub fn iter(&self) -> impl Iterator<Item = &dyn Command>;
}
```

### Binding

`crates/horns-core/src/binding.rs`.

```rust
pub struct BindingEntry {
    pub scope:      BindingScope,
    pub key:        KeyChord,
    pub phase:      Phase,
    pub command_id: CommandId,
}

pub enum BindingScope {
    Anywhere,
    Exact(Path),
    Prefix(Path),
}

pub enum Phase {
    Capture,
    Target,
    Bubble,
}

pub struct CommandId(pub String);                // #[serde(transparent)]
pub struct CommandDisplay { pub name: String, pub description: String }
pub struct CommandScope { pub cursor_path: Option<Path> }

pub struct BindingId(pub String);
pub struct HandlerId(pub String);

pub trait KeyHandler: Send + Sync {
    fn handle(
        &self,
        snapshot: &mut dyn Reader,
        key: &KeyChord,
        ctx: &CommandCtx<'_>,
    ) -> Option<Vec<Write>>;
}

pub struct HandlerEntry {
    pub scope: BindingScope,
    pub phase: Phase,
    pub handler: Arc<dyn KeyHandler>,
}

pub struct HandlerMetadata {
    pub scope: BindingScope,
    pub phase: Phase,
    pub class: String,  // free-form label for introspection
}

pub struct BindingRegistry { /* Vec<BindingEntry>, Vec<HandlerEntry> */ }

impl BindingRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, entry: BindingEntry);
    pub fn register_handler(&mut self, entry: HandlerEntry);
    pub fn lookup(
        &self,
        cursor: &Path,
        key:    &KeyChord,
        phase:  Phase,
    ) -> Option<&CommandId>;
}
```

Specificity (most → least):
1. `scope: Exact(p)`
2. `scope: Prefix(p)`, deeper `p` ahead of shallower
3. `scope: Anywhere`

Discrete bindings win on tie at the same scope+phase against
handlers; handlers fire in registration order after the discrete
tier misses.

### KeyChord

`crates/horns-core/src/key.rs`.

```rust
pub struct KeyChord {
    pub modifiers: KeyModifierSet,
    pub code:      KeyCodeRepr,
}

pub struct KeyModifierSet {
    pub ctrl:   bool,
    pub alt:    bool,
    pub shift:  bool,
    pub super_: bool,
}

pub enum KeyCodeRepr {
    Char(char),
    Enter, Esc, Tab, BackTab, Backspace, Delete,
    Up, Down, Left, Right,
    PageUp, PageDown, Home, End, Insert,
    F(u8),
}
```

`KeyModifierSet::default()` is all-false.

### Subscription

`crates/horns-core/src/subscription.rs` (re-exports the broker-side
traits; see also `crates/ox-broker/src/subscription.rs` for the
underlying broker implementation).

```rust
pub trait Subscription: Send + Sync {
    fn id(&self) -> &SubscriptionId;
    fn watches(&self) -> &[PathPattern];
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write>;
}

pub struct SubCtx<'a> {
    pub snapshot: &'a mut dyn Reader,
    pub change:   &'a PathChange,
    pub spawn:    &'a dyn SpawnHandle,
    pub writer:   Arc<dyn AsyncWriter>,
}

pub trait SpawnHandle: Send + Sync {
    fn spawn(
        &self,
        task: BoxFuture<()>,
    ) -> tokio::task::AbortHandle;
}

pub trait AsyncWriter: Send + Sync {
    fn write(
        &self,
        path: Path,
        record: Record,
    ) -> BoxFuture<Result<Path, StoreError>>;
}
```

Note: `crate::async_store::AsyncWriter` (the existing per-server
trait) and `crate::subscription::AsyncWriter` (this one, shareable
handle) are different traits; they coexist.

### PathPattern

`crates/horns-core/src/subscription.rs`.

```rust
pub enum PathPattern {
    Exact(Path),
    Prefix(Path),
    PrefixSuffix { prefix: Path, suffix: Path },
}

impl PathPattern {
    pub fn matches(&self, path: &Path) -> bool;
}
```

`PrefixSuffix { prefix, suffix }` matches paths whose components
start with `prefix` AND end with `suffix`, with **at least one**
component between them.

Matching is **component-level**, never byte-level.

### Settings UI records

`crates/ox-types/src/settings.rs`.

```rust
pub enum AccountField { Name, Protocol, Endpoint, Auth, Key }
pub enum ModelField { ContextSizeOverride, OutputTokensOverride }

pub struct ModelKey {
    pub account:  String,
    pub model_id: String,
}

pub struct SettingsIndexEntry {
    pub id:            String,
    pub label:         String,
    pub description:   String,
    pub target_cursor: Path,
    pub badge:         BadgeSource,
}

pub enum BadgeSource {
    None,
    Static(String),
    SubtreeCount(Path),
    PrimaryReference,
}

pub struct ValidationDiagnostics {
    pub field_errors:   BTreeMap<AccountField, String>,
    pub computed_at_ms: u64,
}

pub enum GlobalBanner {
    None,
    Error { message: String, set_at_ms: u64 },
    Info  { message: String, set_at_ms: u64 },
}
```

### Gate-domain records

`crates/ox-types/src/completion_role.rs` and `model_info.rs` (moved
to ox-types in O0 to break a dependency cycle).

```rust
pub struct CompletionRole {
    pub account:  String,
    pub model_id: String,
}

pub struct ModelInfo {
    pub id:                String,
    pub display_name:      String,
    pub max_context_size:  Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub source:            ModelInfoSource,
}

pub enum ModelInfoSource { Server, KnownTable, UserOverride }
```

`crates/ox-gate/src/`:

```rust
pub struct ApiKey(pub String);  // newtype, Debug elides body

pub enum AccountTestStatus {
    Idle,
    Testing { started_at_ms: u64 },
    Success { dialect: String,
              latency_ms: u64,
              completed_at_ms: u64 },
    Failed  { reason: String, completed_at_ms: u64 },
}

pub enum CatalogRefreshStatus {
    Idle,
    Refreshing { started_at_ms: u64 },
    Success { models_added: u32,
              models_updated: u32,
              completed_at_ms: u64 },
    Failed  { reason: String, completed_at_ms: u64 },
}
```

## Paths

### Settings namespace (display tree)

Cursors, focus, selection, and UI-mode state.

| Path | Type | Meaning |
|---|---|---|
| `ui/settings/cursor` | `Path` | Currently-displayed *page*. Page navigation only. |
| `ui/settings/focused` | `Path` | Universal focus authority. Encodes the focused row, compound widget, or widget sub-element. Its ancestor chain is the dispatch scope path. See `architecture.md` §Cursor as universal focus. |
| `ui/settings/_request_exit` | `bool` | Cross-component signal: event loop reads it to switch screens. |
| `ui/settings/expanded` | `Vec<String>` | Set (as list) of expanded accordion entries. |
| `ui/settings/accounts/selected` | `Option<String>` | Currently selected account name. |
| `ui/settings/models/selected` | `Option<ModelKey>` | Currently selected (account, model) pair. |
| `ui/global/banner` | `GlobalBanner` | App-wide banner (errors, info). |
| `settings/index/entries/{id}` | `SettingsIndexEntry` | Index page row metadata. |

Compound widget engagement — the cursor (`ui/settings/focused`)
under one of these synthetic namespaces means the widget is active.
Each widget has a sibling working-state subtree at
`ui/settings/<widget>` carrying its buffer, saved pre-open cursor,
and any staged drafts:

| Synthetic cursor namespace | Widget | Working state subtree |
|---|---|---|
| `settings/_compose_form/{name,protocol,key,...}` | Composing a new account | `ui/settings/new_account/{buffer, protocol, key, errors, cursor_saved}` |
| `settings/_confirm_delete` | Confirming a delete | `ui/settings/pending_delete/{target_account, cursor_saved}` |
| `settings/_edit` | Inline-editing a field | `ui/settings/edit/{target_path, buffer, cursor_saved}` |
| `settings/_manual_model/{id,ctx,out}` | Manual model entry | `ui/settings/manual_model/{buffer, account, staged_id, staged_ctx, cursor_saved}` |

Retired discriminator paths (do not reintroduce — replaced by the
cursor sitting under the corresponding `_<widget>` namespace):

- `ui/settings/new_account/active: bool` → cursor at
  `settings/_compose_form/<leaf>`.
- `ui/settings/manual_model/stage: ManualModelStage` → cursor's
  leaf segment under `settings/_manual_model`.
- `ui/settings/pending_delete: Option<AccountName>` (the
  value-flag form) → cursor at `settings/_confirm_delete`; target
  moved to the sibling `target_account` child.
- `ui/settings/edit_mode: bool` + `edit_field_path: Option<Path>` →
  cursor at `settings/_edit`; the edited field moved to
  `ui/settings/edit/target_path`.

### Config namespace (data tree)

The world's actual state. Writes here change the world; reads
project it.

| Path | Type | Meaning |
|---|---|---|
| `config/gate/accounts/{name}` | `AccountConfig` | Per-account record. *A write here creates the account; a `Null` write deletes it.* |
| `config/gate/accounts/{name}/models` | `Vec<ModelInfo>` | Catalog |
| `config/gate/accounts/{name}/test_status` | `AccountTestStatus` | Connectivity test outcome |
| `config/gate/accounts/{name}/refresh_status` | `CatalogRefreshStatus` | Catalog refresh outcome |
| `config/gate/accounts/{name}/validation_status` | `ValidationDiagnostics` | Per-field validation diagnostics |
| `config/gate/providers/{name}` | `ProviderConfig` | endpoint+dialect |
| `config/gate/completions/primary` | `CompletionRole` | (account, model) |
| `config/save` | `Null` | Async trigger: persist runtime config to disk. |

Per-instance async action triggers (write `Null` to trigger an
async-only action; subscription performs the work):

| Path | Subscription | Why a trigger |
|---|---|---|
| `config/gate/accounts/{name}/test_now` | `AccountTestSubscription` | Network call; no synchronous form. |
| `config/gate/accounts/{name}/refresh_now` | `CatalogRefreshSubscription` | Network call; no synchronous form. |

There is **no** `config/gate/accounts/_create_now` and **no**
`config/gate/accounts/{name}/delete_now`. Account creation is a
direct write to `config/gate/accounts/{name}` carrying the
`AccountConfig`. Deletion is a `Null` write to the same path. Any
async or cross-cutting follow-up (catalog fetch on create, side-data
cleanup on delete) lives in a reactive subscription watching
`Prefix(config/gate/accounts)`.

### Secret namespace

| Path | Type | Meaning |
|---|---|---|
| `secret/keys/{name}` | `ApiKey` | Per-account API key |

Mounted separately from `config/`; backed by `keys.json` with
`chmod 0600`.

## Subscriptions

### `AccountTestSubscription` (async action trigger)

- Watches: `PrefixSuffix { prefix: config/gate/accounts,
  suffix: test_now }`
- Reads the AccountConfig + ApiKey; writes
  `test_status: Testing` synchronously; spawns
  `transport.test_connection`; writes `Success`/`Failed` from the
  spawned task. Network call, no synchronous form, hence the
  `_now` trigger pattern.
- Holds `Mutex<HashMap<String, AbortHandle>>` for supersession.

### `CatalogRefreshSubscription` (async action trigger)

- Watches: `PrefixSuffix { prefix: config/gate/accounts,
  suffix: refresh_now }`
- Reads the AccountConfig + ApiKey; writes
  `refresh_status: Refreshing`; spawns `transport.fetch_catalog`; on
  success writes the new `Vec<ModelInfo>` to `…/models` plus
  `refresh_status: Success { models_added, models_updated }`. On
  failure writes `Failed { reason }` and does **not** clobber the
  existing models.
- Falls back to `known_family_metadata` for models with absent
  `max_*_tokens`, setting `source: KnownTable`.
- Holds the same supersession map shape as `AccountTestSubscription`.

### `AccountDeleteCleanupSubscription` (reactive observer)

- Watches: `Prefix(config/gate/accounts)`, filtering for null writes
  at the account-record depth (`prefix.len() + 1`).
- Cleans up side data: drops the matching `secret/keys/<name>`,
  drops the provider record at `config/gate/providers/<name>` if no
  other account references it, clears
  `ui/settings/accounts/selected` if it matched the deleted name.
- The actual delete (the `Null` write to
  `config/gate/accounts/<name>`) was already performed by the CLI;
  this subscription does the cross-cutting follow-up.

### `CatalogFetchOnCreateSubscription` (reactive observer)

- Watches: `Prefix(config/gate/accounts)`, filtering for new entries
  at the account-record depth (`change.before.is_none()`).
- Spawns a catalog fetch for the newly-created account. Same shape
  as `CatalogRefreshSubscription` but triggered by the existence of
  a new account record rather than by a `_now` trigger.

### `ConfigSaveSubscription` (async action trigger)

- Watches: `Exact(config/save)`
- The actual save runs in `ConfigStore::save_runtime` (driven by the
  ConfigStore mount's Writer impl when it sees a write at `save`).
  File IO, hence the trigger pattern. The subscription itself logs
  that the trigger was observed.

## Day-one commands

In `crates/ox-cli/src/settings/commands/`:

`highlight.rs` — 6 commands (next/prev × 3 areas):

- `highlight.index.{next,prev}`
- `highlight.accounts.{next,prev}`
- `highlight.models.{next,prev}`

`navigation.rs` — 4:

- `nav.descend.index`
- `nav.descend.accounts`
- `nav.descend.models`
- `nav.ascend`

`account_model.rs`:

- `accounts.add` (opens the inline name-prompt mode)
- `accounts.delete_confirm` (opens the inline confirmation mode)
- `account.test`, `account.refresh` (write `test_now` / `refresh_now`)
- `models.set_primary`
- `app.save` (writes `config/save`)
- `field.account.{next,prev}`, `field.model.{next,prev}`
- `selector.cycle.protocol`, `selector.cycle.auth`

`edit.rs` (inline-edit buffer commands; printable input is claimed
by a `TextInputHandler` at the same `_edit` scope, Backspace + Enter
+ Esc by discrete bindings routed to these commands):

- `edit.commit`, `edit.cancel`

There is no `accounts.create` or `accounts.delete` command — those
were modal-era RPC translation steps. In the current shape, the
"create" action lives inside `accounts.compose.commit`'s handler
and performs a direct write to `config/gate/accounts/<name>`. The
"delete" action lives inside the `pending_delete.commit` handler
(bound to `y` at `Exact(settings/_confirm_delete)`) and performs a
direct `Null` write to the same path.

## Day-one bindings

See `crates/ox-cli/src/settings/bindings.rs::register`. Bindings are
indexed by scope (`BindingScope::Exact(<path>)`) and phase. The
dispatcher walks the focus cursor's ancestor chain and queries each
scope at each phase.

Page-level (registered at `Exact(settings)` so they ride at Bubble
under any cursor on the settings screen):

- `j`/`k` row highlight; `Enter` toggle expansion / activate;
  `a` open compose-new (writes cursor to
  `settings/_compose_form/name`); `d` open delete-confirm;
  `t` test selected; `r` refresh; `P` set bootstrap; `Esc` ascend
  / collapse; `?` help; `Ctrl+S` save.

Compound widget scopes (registered at the synthetic cursor
namespace; appear on the dispatch path iff the cursor descends
into the namespace):

- `Exact(settings/_compose_form)` — lifecycle keys (`Esc` cancel,
  `Tab`/`Shift+Tab` advance) at Capture; `Enter` commit at Bubble.
- `Exact(settings/_compose_form/<leaf>)` (`name`, `protocol`,
  `key`, ...) — per-leaf Target bindings (printable ASCII for Text
  leaves, `h`/`l` for Selector leaves).
- `Exact(settings/_confirm_delete)` — `Esc`/`n` cancel at Capture;
  `y` commit at Target/Bubble.
- `Exact(settings/_edit)` — printable ASCII → `edit.insert_char`
  at Target; Backspace → `edit.delete_back`; Enter → `edit.commit`
  at Bubble; Esc → `edit.cancel` at Capture. ASCII uppercase
  letters bind with `shift_only()` modifiers.
- `Exact(settings/_manual_model)` — lifecycle keys at Capture;
  `Enter` advance / commit at Bubble.
- `Exact(settings/_manual_model/<leaf>)` (`id`, `ctx`, `out`) —
  per-leaf Target bindings.

The dispatcher reads `ui/settings/focused`, walks its ancestor
chain, and queries each scope at each phase. There are no
discriminator reads anywhere in dispatch — engagement is encoded
structurally by the cursor's position.

## File map

### horns crates (the framework itself)

```
crates/horns/                          # umbrella crate: re-exports
  src/lib.rs                           # horns-core; exposes
                                       # horns::ratatui under the
                                       # `ratatui` feature.
  docs/                                # framework documentation
    ui_framework.md                    # 60-second pitch + index
    architecture.md                    # mental model
    howto.md                           # task-oriented recipes
    reference.md                       # this file

crates/horns-core/src/
  lib.rs                               # public re-exports
  view.rs                              # the View enum + supporting types
  key.rs                               # KeyChord, KeyCodeRepr,
                                       # KeyModifierSet
  binding.rs                           # BindingEntry, BindingScope,
                                       # Phase, BindingRegistry,
                                       # KeyHandler, HandlerEntry,
                                       # HandlerMetadata
  command.rs                           # Command trait, CommandRegistry,
                                       # CommandCtx, CommandMetadata
  render.rs                            # Renderer trait,
                                       # RendererRegistry, RenderCtx,
                                       # AscendRule, RendererMetadata,
                                       # Rect
  dispatch.rs                          # Dispatcher: three-phase walk
                                       # of the cursor's ancestor chain
  install.rs                           # install / build_install_bundle /
                                       # build_install_bundle_from_registries;
                                       # KeyDispatch / Render /
                                       # ThemeChange subscriptions
  subscription.rs                      # Subscription trait, SubCtx,
                                       # PathPattern, PathChange,
                                       # SubscriptionRegistry,
                                       # SpawnHandle, AsyncWriter
  write.rs                             # Write { path, record }
  path_serde.rs                        # serde adapter for structfs Path

crates/horns-ratatui/src/
  lib.rs                               # re-exports
  render.rs                            # render_to_frame: the View →
                                       # ratatui translator
                                       # (the only ratatui-touching
                                       # point in the framework)
  theme.rs                             # Theme type used by render.rs
```

### Settings worked-example crates (the framework's first user)

```
crates/ox-broker/src/
  subscription.rs                      # DispatchingStore wiring;
                                       # horns re-exports the traits
  dispatching_store.rs                 # cascade-bounded
                                       # write-and-dispatch
  client.rs                            # ClientHandle::read_subtree

crates/ox-cli/src/
  dispatch.rs                          # send_key helper for the host's
                                       # event loop (encodes crossterm
                                       # keys, writes to <input_path>/key)
  settings/
    mod.rs                             # settings::install — calls
                                       # horns::build_install_bundle_from_registries
                                       # against bindings/commands/renderers
                                       # registered below
    snapshot.rs                        # SettingsSnapshot +
                                       # fetch_settings_view_state
                                       # (pre-broker-mount Reader)
    bootstrap.rs                       # populate_index_entries,
                                       # maybe_first_run_cursor,
                                       # detect_legacy_settings
    bindings.rs                        # day-one binding table for
                                       # the settings screen
    renderers/
      mod.rs                           # register_all
      util.rs                          # read_typed, child_names_under,
                                       # subtree_count
      index.rs                         # accordion (reads UI-state +
                                       # data tree, composes affordances)
    commands/
      mod.rs                           # command! macro + register_all
      highlight.rs                     # navigation highlights
      navigation.rs                    # cursor moves + path_to_value /
                                       # path_from_value helpers
      account_model.rs                 # account / model actions
      edit.rs                          # inline-edit buffer commands
                                       # (commit, cancel; printable
                                       # input claimed by a
                                       # TextInputHandler)
      tree.rs                          # tree.activate (Enter dispatch)

crates/ox-gate/src/
  api_key.rs                           # ApiKey newtype
  completion_role.rs                   # re-export from ox-types
  model_info.rs                        # re-export from ox-types
  account_test_status.rs               # AccountTestStatus enum
  catalog_refresh_status.rs            # CatalogRefreshStatus enum
  known_family.rs                      # KnownFamilyEntry +
                                       # known_family_metadata
  transport.rs                         # Transport trait + HttpTransport
  validation.rs                        # validate_account
  subscriptions/
    mod.rs                             # register_all (settings-screen
                                       # subscriptions; separate from
                                       # the horns runtime subscriptions)
    util.rs                            # path helpers,
                                       # read_typed_via_reader,
                                       # MockTransport (cfg(test))
    account_test.rs
    catalog_refresh.rs
    account_delete.rs
    account_create.rs
    config_save.rs

crates/ox-types/src/                   # shared typed records used by
                                       # the settings worked example
  settings.rs                          # SettingsIndexEntry, AccountField,
                                       # ModelField, ModelKey, banners
  completion_role.rs                   # CompletionRole
  model_info.rs                        # ModelInfo, ModelInfoSource
```

## Glossary

- **Focus cursor** — Path at `<cursor_path>` (configured per
  `horns::install`) naming what is currently focused. Its ancestor
  chain is the dispatch scope path. The cursor can sit at a row, a
  compound widget root, or a widget sub-element.
- **Page cursor** — Optional secondary cursor a host can use to
  distinguish "the page the user is reading" from focus. The
  settings worked example uses `ui/settings/cursor` for page
  navigation; the framework only requires the focus cursor.
- **Compound widget mode** — A state the user has entered within a
  page (composing a name, confirming a delete, editing a field
  inline). Encoded by where the focus cursor sits — under
  `<page>/_<widget>/<leaf>` — not by a separate discriminator flag.
  Working state for the engaged widget (typed buffer, saved
  pre-open cursor, staged drafts) lives at named UI-state paths.
  Widgets are dismissed by restoring the saved cursor and cascade-
  clearing the working-state subtree.
- **Renderer** — Pure `&mut dyn Reader → View` function, registered
  against a cursor path. Reads both data-tree and display-tree
  (UI-state) state.
- **Command** — Pure `&mut dyn Reader, &CommandCtx → Vec<Write>`
  function, registered by `CommandId`. Performs data-tree writes
  directly when the work is synchronous; writes a trigger path only
  when the work is fundamentally async.
- **Binding** — `BindingEntry` mapping `(scope, key, phase) →
  CommandId`. Introspectable.
- **Handler** — `KeyHandler` (opaque) registered at a (scope, phase)
  to claim bulk input (e.g. every printable ASCII char). Asked
  after the discrete-binding tier misses at the same scope+phase.
- **Subscription** — Broker-side handler that fires on writes
  matching its `PathPattern`. Either a *reactive observer* (watches
  a real data path and does async/cross-cutting follow-up) or an
  *async action trigger* (watches a `…/<verb>_now` path and performs
  the requested async work). Never an RPC translator.
- **Mount** — A horns instance installed on the broker via
  `horns::install`. Each call registers three subscriptions
  (KeyDispatch, Render, ThemeChange) plus metadata writes; a host
  may install multiple horns mounts at disjoint prefixes for
  multi-screen UIs.
- **Snapshot** — In-memory Reader populated by walking the broker's
  data; consumed by renderers and commands. (The horns
  subscriptions' `SubCtx` provides a live Reader over the broker;
  the settings worked example also uses a prefetched
  `SettingsSnapshot` for its pre-broker-mount render path.)
- **Ascend rule** — A renderer's `NearestRegistered` or `ExitScreen`
  policy for Esc.
- **Cascade bound** — Maximum recursion depth of
  subscription-triggered writes (default 64).
- **`_request_exit`** — Cross-component signal (settings worked
  example: `ui/settings/_request_exit`) written by `NavAscend` at
  `ExitScreen`; the host's event loop reads it next iteration and
  switches screens. One of the framework's few legitimate sentinel
  paths — there is no data-tree home for "please exit."
- **Action trigger path** — A path like `…/test_now` or
  `…/refresh_now` whose Null-write means "please perform this
  async-only action on this instance." Subscription does the work.
  Used only when the action has no synchronous form.
- **Display tree** — The portion of the namespace carrying UI state
  (cursors, selection, edit buffers). Distinct from the data tree
  which carries facts about the world. Both share the broker
  namespace; subscriptions watch data-tree paths only.
- **Data tree** — The portion of the namespace carrying facts about
  the world. A write here changes the world; a `Null` write deletes
  the named record.
