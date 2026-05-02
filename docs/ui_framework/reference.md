# UI framework — reference

Lookup-only. Type signatures, paths, the file map, and a glossary.

## Types

### View

`crates/ox-view/src/lib.rs`. No serde, no ratatui.

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

`crates/ox-cli/src/settings/registry.rs`.

```rust
pub trait Renderer: Send + Sync {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View;
    fn ascend_to(&self) -> AscendRule;
}

pub struct RenderCtx<'a> {
    pub area:     Rect,
    pub data:     &'a mut dyn Reader,
    pub registry: &'a RendererRegistry,
    pub theme:    &'a Theme,
}

pub enum AscendRule {
    NearestRegistered,
    ExitScreen,
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

`crates/ox-cli/src/settings/command_registry.rs`.

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

`crates/ox-cli/src/settings/binding_registry.rs` (the registry itself)
and `crates/ox-types/src/command_binding.rs` (the data shape).

```rust
pub struct BindingEntry {
    pub screen:      Screen,
    pub cursor_path: Option<Path>,
    pub mode:        Option<Mode>,
    pub key:         KeyChord,
    pub command_id:  CommandId,
}

pub struct CommandId(pub String);                // #[serde(transparent)]
pub struct CommandDisplay { pub name: String, pub description: String }
pub struct CommandScope {
    pub screen:      Screen,
    pub cursor_path: Option<Path>,
}

pub struct BindingRegistry { /* Vec<BindingEntry> */ }

impl BindingRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, entry: BindingEntry);
    pub fn lookup(
        &self,
        screen: Screen,
        cursor: &Path,
        mode:   Option<Mode>,
        key:    &KeyChord,
    ) -> Option<&CommandId>;
}
```

Specificity (most → least):
1. cursor: Some + mode: Some
2. cursor: Some + mode: None
3. cursor: None + mode: Some
4. cursor: None + mode: None

### KeyChord

`crates/ox-types/src/key_chord.rs`.

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

`crates/ox-broker/src/subscription.rs`.

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

`crates/ox-types/src/subscription.rs`.

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

pub struct CreateAccountRequest { pub name: String }
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

### Settings namespace

| Path | Type | Meaning |
|---|---|---|
| `ui/settings/cursor` | `Path` | Currently-displayed page |
| `ui/settings/_request_exit` | `bool` | Set to `true` to exit screen |
| `ui/settings/index/selected` | `usize` | Index page selection |
| `ui/settings/accounts/selected` | `Option<String>` | Account name |
| `ui/settings/models/selected` | `Option<ModelKey>` | (account, model) |
| `ui/settings/account_detail/field` | `AccountField` | Focused row |
| `ui/settings/model_detail/field` | `ModelField` | Focused row |
| `ui/settings/edit_cursor` | `u32` | Char cursor in text field |
| `ui/settings/new_account/name_input` | `String` | Draft name |
| `ui/global/banner` | `GlobalBanner` | App-wide banner |
| `settings/index/entries/{id}` | `SettingsIndexEntry` | Index row |

### Config namespace

| Path | Type | Meaning |
|---|---|---|
| `config/gate/accounts/{name}` | `AccountConfig` | Per-account record |
| `config/gate/accounts/{name}/models` | `Vec<ModelInfo>` | Catalog |
| `config/gate/accounts/{name}/test_status` | `AccountTestStatus` | |
| `config/gate/accounts/{name}/refresh_status` | `CatalogRefreshStatus` | |
| `config/gate/accounts/{name}/validation_status` | `ValidationDiagnostics` | |
| `config/gate/providers/{name}` | `ProviderConfig` | endpoint+dialect |
| `config/gate/completions/primary` | `CompletionRole` | (account, model) |
| `config/save` | `Null` | Triggers save subscription |

Per-instance action paths (write `Null` to trigger):

| Path | Subscription |
|---|---|
| `config/gate/accounts/{name}/test_now` | `AccountTestSubscription` |
| `config/gate/accounts/{name}/refresh_now` | `CatalogRefreshSubscription` |
| `config/gate/accounts/{name}/delete_now` | `AccountDeleteSubscription` |
| `config/gate/accounts/_create_now` | `AccountCreateSubscription` |

(`_create_now` takes a typed `CreateAccountRequest` payload, not Null.)

### Secret namespace

| Path | Type | Meaning |
|---|---|---|
| `secret/keys/{name}` | `ApiKey` | Per-account API key |

Mounted separately from `config/`; backed by `keys.json` with
`chmod 0600`.

## Subscriptions

### `AccountTestSubscription`

- Watches: `PrefixSuffix { prefix: config/gate/accounts,
  suffix: test_now }`
- Validates the AccountConfig; writes `test_status: Testing`
  synchronously; spawns `transport.test_connection`; writes
  `Success`/`Failed` from the spawned task.
- Holds `Mutex<HashMap<String, AbortHandle>>` for supersession.

### `CatalogRefreshSubscription`

- Watches: `PrefixSuffix { prefix: config/gate/accounts,
  suffix: refresh_now }`
- Validates; writes `refresh_status: Refreshing`; spawns
  `transport.fetch_catalog`; on success writes the new
  `Vec<ModelInfo>` to `…/models` plus
  `refresh_status: Success { models_added, models_updated }`. On
  failure writes `Failed { reason }` and does **not** clobber the
  existing models.
- Falls back to `known_family_metadata` for models with absent
  `max_*_tokens`, setting `source: KnownTable`.
- Holds the same supersession map shape as `AccountTestSubscription`.

### `AccountDeleteSubscription`

- Watches: `PrefixSuffix { prefix: config/gate/accounts,
  suffix: delete_now }`
- Fully synchronous; no spawn. Returns one `Vec<Write>` removing the
  account record + key + provider entry, clearing selection if it
  matched, and popping the cursor to `settings/accounts`.

### `AccountCreateSubscription`

- Watches: `Exact(config/gate/accounts/_create_now)`
- Reads `change.after` as `CreateAccountRequest { name }`; validates
  via `PathComponent::try_new`; on success writes a default
  `AccountConfig`, sets `ui/settings/accounts/selected: Some(name)`,
  and writes the cursor to `settings/accounts/_detail`. On invalid
  name writes a `GlobalBanner::Error`.

### `ConfigSaveSubscription`

- Watches: `Exact(config/save)`
- The actual save runs in `ConfigStore::save_runtime` (driven by the
  ConfigStore mount's Writer impl when it sees a write at `save`).
  This subscription exists for protocol uniformity — it logs that
  the trigger was observed.

## Day-one commands

27 commands in `crates/ox-cli/src/settings/commands/`:

`highlight.rs` — 6 commands (next/prev × 3 areas):

- `highlight.index.{next,prev}`
- `highlight.accounts.{next,prev}`
- `highlight.models.{next,prev}`

`navigation.rs` — 4:

- `nav.descend.index`
- `nav.descend.accounts`
- `nav.descend.models`
- `nav.ascend`

`account_model.rs` — 17:

- `accounts.add`, `accounts.delete_confirm`, `accounts.cancel`
- `accounts.create`, `accounts.delete`
- `account.test`, `account.refresh`
- `models.set_primary`
- `app.save`
- `field.account.{next,prev}`, `field.model.{next,prev}`
- `field.insert`, `field.delete_back`
- `selector.cycle.protocol`, `selector.cycle.auth`

## Day-one bindings

See `crates/ox-cli/src/settings/bindings.rs::register`. Indexed by
cursor scope. The text-editing scope (`settings/accounts/_detail`)
gets ~96 entries from `register_text_editing` covering printable
ASCII + Backspace.

Per spec §6:

- `settings/index`: `j`/`k` highlight; `Enter` descend; `Esc` ascend
- `settings/accounts`: `j`/`k` + `Enter` + `a` (add) + `d` (delete) + `Esc`
- `settings/accounts/_detail`: Tab/Down/Shift+Tab/Up cycle field;
  `t` test; `Ctrl+s` save; `Esc` ascend; printable + Backspace
  text-edit
- `settings/accounts/_new`: `Enter` create; `Esc` cancel
- `settings/accounts/_delete`: `y` delete; `n`/`Esc` cancel
- `settings/models`: `j`/`k` + `Enter` + `P` set-primary + `r`
  refresh + `Esc`
- `settings/models/_detail`: Tab cycle; `Esc` ascend

## File map

```
crates/ox-view/                        # the View enum
  src/lib.rs                           # all View types + constructors

crates/ox-types/src/                   # shared typed records
  command_binding.rs                   # CommandId, BindingEntry, etc.
  key_chord.rs                         # KeyChord, KeyCodeRepr
  settings.rs                          # SettingsIndexEntry, AccountField,
                                       # ModelField, ModelKey, banners,
                                       # CreateAccountRequest
  subscription.rs                      # PathPattern, PathChange, Write,
                                       # SubscriptionId
  completion_role.rs                   # CompletionRole
  model_info.rs                        # ModelInfo, ModelInfoSource
  path_serde.rs                        # serde adapter for structfs Path

crates/ox-broker/src/
  subscription.rs                      # Subscription trait, SubCtx,
                                       # SpawnHandle, AsyncWriter,
                                       # SubscriptionRegistry
  dispatching_store.rs                 # DispatchingStore: cascade-bounded
                                       # write-and-dispatch
  client.rs                            # ClientHandle::read_subtree

crates/ox-cli/src/
  view_render.rs                       # View → ratatui translator
                                       # (the only ratatui-touching point
                                       # at the renderer boundary)
  dispatch.rs                          # send_key with optional cursor
                                       # + registries
  settings/
    mod.rs                             # submodule layout
    registry.rs                        # Renderer trait, RendererRegistry,
                                       # RenderCtx, AscendRule
    command_registry.rs                # Command trait, CommandRegistry,
                                       # CommandCtx
    binding_registry.rs                # BindingRegistry with specificity
    dispatch.rs                        # dispatch_settings_key
    snapshot.rs                        # SettingsSnapshot
                                       # + fetch_settings_view_state
    bootstrap.rs                       # populate_index_entries,
                                       # maybe_first_run_cursor,
                                       # detect_legacy_settings
    bindings.rs                        # day-one binding table
    renderers/
      mod.rs                           # register_all
      util.rs                          # read_typed, child_names_under,
                                       # subtree_count
      index.rs
      accounts_list.rs
      account_detail.rs
      models_list.rs
      model_detail.rs
      overlay_new_account.rs
      overlay_delete_account.rs
    commands/
      mod.rs                           # command! macro + register_all
      highlight.rs                     # 6 commands
      navigation.rs                    # 4 commands + path_to_value /
                                       # path_from_value helpers
      account_model.rs                 # 17 commands

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
    mod.rs                             # register_all
    util.rs                            # path helpers,
                                       # read_typed_via_reader,
                                       # MockTransport (cfg(test))
    account_test.rs
    catalog_refresh.rs
    account_delete.rs
    account_create.rs
    config_save.rs
```

## Glossary

- **Cursor** — Path at `ui/settings/cursor` naming the
  currently-displayed page.
- **Renderer** — Pure `&mut dyn Reader → View` function, registered
  against a cursor path.
- **Command** — Pure `&mut dyn Reader, &CommandCtx → Vec<Write>`
  function, registered by `CommandId`.
- **Binding** — `BindingEntry` mapping
  `(screen, cursor, mode, key) → CommandId`.
- **Subscription** — Broker-side handler that fires on writes
  matching its `PathPattern`. May return more writes
  (cascade-bounded) or spawn async work.
- **Snapshot** — In-memory Reader populated by walking the broker's
  data; consumed by renderers and commands.
- **Ascend rule** — A renderer's `NearestRegistered` or `ExitScreen`
  policy for Esc.
- **Cascade bound** — Maximum recursion depth of
  subscription-triggered writes (default 64).
- **`_request_exit`** — Sentinel boolean at
  `ui/settings/_request_exit` written by `NavAscend` at `ExitScreen`;
  the event loop reads it next iteration and switches screens.
- **Action path** — A path like `…/test_now` or `…/_create_now` that
  exists only to trigger a subscription. Writing `Null` (or a typed
  payload for `_create_*` paths) fires the handler.
- **Display tree** — The conceptual tree of "what's currently on
  screen", inferred from cursor + data. Not a separate datastructure.
