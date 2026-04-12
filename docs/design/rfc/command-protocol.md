# RFC: Command Pattern & Protocol

**Status:** Draft
**Date:** 2026-04-11

## Problem

Ox has actions scattered across multiple stores (UiStore, ApprovalStore,
TextInputStore) addressed by raw StructFS path strings. Keybindings in
`bindings.rs` reference these paths directly with inline parameter maps.
This creates several problems:

1. **No discoverability.** There is no catalog of what the system can do.
   A user or script cannot enumerate available commands.

2. **No validation.** A binding can reference a nonexistent path or pass
   wrong parameters. Errors surface at runtime as store write failures.

3. **No commandline.** Users cannot type commands — all interaction goes
   through hardcoded keybindings. There is no `:compose` equivalent.

4. **No portable command vocabulary.** ox-cli and ox-web both need to
   invoke the same actions but have no shared definition of what those
   actions are or what parameters they take.

## Design

The command system has two layers:

- **Static Rust types** (`CommandDef`, `ParamDef`, etc.) that define
  commands at compile time with type-safe parameter schemas. These are
  serializable, so they can cross platform boundaries (wasm ↔ JS,
  store reads, help rendering).

- **A StructFS store** (`CommandStore`) that holds the registry at
  runtime. Reading it discovers commands. Writing to it invokes them.
  The store is the dynamic surface; the static types are the data it
  holds.

### Static command types

#### CommandDef

A **command definition** is metadata about a single action the system
can perform.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDef {
    /// Unique identifier, used in commandline and scripting.
    pub name: String,
    /// StructFS path this command writes to.
    pub target: String,
    /// Parameter schema.
    pub params: Vec<ParamDef>,
    /// Human-readable summary.
    pub description: String,
    /// Whether this command appears in help/commandline.
    pub user_facing: bool,
}
```

Uses owned `String` fields so instances can be serialized to/from
StructFS Values, JSON, or any serde format. Built-in commands are
constructed from `&'static` data at startup via const helper functions
or a declarative macro, but the type itself is owned for portability.

#### ParamDef

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDef {
    /// Parameter name (key in the args map).
    pub name: String,
    /// Expected value type.
    pub kind: ParamKind,
    /// Whether the parameter must be provided.
    pub required: bool,
    /// Default value if omitted (must match `kind`).
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParamKind {
    String,
    Integer,
    Bool,
    /// One of a fixed set of string values.
    Enum(Vec<String>),
}
```

`default` uses StructFS `Value` directly — no separate `ParamDefault`
enum. This keeps serialization uniform: a command definition round-trips
through `Value` without special handling.

#### CommandInvocation

A concrete request to execute a command with bound parameters.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandInvocation {
    pub command: String,
    pub args: BTreeMap<String, Value>,
}
```

All input surfaces (keybindings, commandline, macros, scripts) produce
`CommandInvocation` values.

#### Static definition helpers

Built-in commands are defined as `&'static` data using const helpers:

```rust
pub struct StaticCommandDef {
    pub name: &'static str,
    pub target: &'static str,
    pub params: &'static [StaticParamDef],
    pub description: &'static str,
    pub user_facing: bool,
}

pub struct StaticParamDef {
    pub name: &'static str,
    pub kind: StaticParamKind,
    pub required: bool,
}

pub enum StaticParamKind {
    String,
    Integer,
    Bool,
    Enum(&'static [&'static str]),
}

impl StaticCommandDef {
    pub fn to_command_def(&self) -> CommandDef { /* convert */ }
}
```

This gives compile-time definitions without allocation. The `to_command_def()`
conversion allocates once at registry population time.

A catalog function collects all built-ins:

```rust
pub fn builtin_commands() -> &'static [StaticCommandDef] {
    &[
        StaticCommandDef {
            name: "compose",
            target: "ui/enter_insert",
            params: &[StaticParamDef {
                name: "context",
                kind: StaticParamKind::Enum(&["compose", "reply", "search"]),
                required: true,
            }],
            description: "Open compose input",
            user_facing: true,
        },
        // ... all other commands
    ]
}
```

### CommandStore

The command registry is a StructFS store, mounted in the broker namespace.
It holds the registry internally and exposes it through the standard
Reader/Writer interface.

#### Reads — discovery

| Path | Returns |
|------|---------|
| `""` or `commands` | Array of all command definitions as Value::Map |
| `commands/{name}` | Single command definition as Value::Map |
| `commands?user_facing=true` | Filtered list (user-facing only) |
| `schema/{name}` | Parameter schema for a command |

A serialized `CommandDef` as Value looks like:

```json
{
  "name": "compose",
  "target": "ui/enter_insert",
  "params": [
    {
      "name": "context",
      "kind": {"Enum": ["compose", "reply", "search"]},
      "required": true,
      "default": "compose"
    }
  ],
  "description": "Open compose input",
  "user_facing": true
}
```

This is what a help screen, shortcuts viewer, or commandline
tab-completer reads to discover available commands and their shapes.
Because `CommandDef` is `Serialize`, the conversion to `Value` is
mechanical via `structfs_serde_store::to_value()`.

#### Writes — invocation and registration

| Path | Data | Effect |
|------|------|--------|
| `invoke` | `CommandInvocation` as Value::Map | Validate, resolve, dispatch to target store |
| `register` | `CommandDef` as Value::Map | Add a command to the registry |
| `unregister` | `{name: String}` | Remove a command from the registry |

**Invoke flow:**

1. Deserialize the write value into `CommandInvocation`
2. Look up command by name in the internal registry
3. Validate args against param schema (type check, required check,
   enum value check)
4. Apply defaults for omitted optional params
5. Build `Record::parsed(Value::Map(validated_args))`
6. Dispatch write to `Path::parse(def.target)` through the broker

The CommandStore holds a broker `ClientHandle` (or equivalent dispatcher)
to forward resolved writes to target stores. This is the same pattern
the InputStore already uses for its `CommandDispatcher`.

**Register flow:**

1. Deserialize the write value into `CommandDef`
2. Validate name uniqueness
3. Add to internal registry

This supports plugins that mount new stores and want to expose commands
for them. The plugin mounts its store, then writes to `command/register`
with definitions for the commands the store handles.

#### Internal structure

```rust
pub struct CommandStore {
    registry: CommandRegistry,
    dispatcher: Option<CommandDispatcher>,
}

pub struct CommandRegistry {
    commands: Vec<CommandDef>,
    by_name: HashMap<String, usize>,
}

impl CommandRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, def: CommandDef) -> Result<(), CommandError>;
    pub fn get(&self, name: &str) -> Option<&CommandDef>;
    pub fn iter(&self) -> impl Iterator<Item = &CommandDef>;
    pub fn user_facing(&self) -> impl Iterator<Item = &CommandDef>;

    /// Validate and resolve an invocation to a target path + record.
    pub fn resolve(
        &self,
        invocation: &CommandInvocation,
    ) -> Result<(Path, Record), CommandError>;
}
```

The `CommandRegistry` is a plain Rust struct — no StructFS dependency.
The `CommandStore` wraps it with Reader/Writer. This means the registry
logic is independently testable and usable outside the store context
(e.g. in ox-web where StructFS stores are wrapped in `Rc<RefCell>`).

### Integration with InputStore

The InputStore currently resolves bindings and dispatches directly to
target paths. With the CommandStore, it dispatches to `command/invoke`
instead.

**Before:**
```
key event → InputStore resolves binding → dispatches write to "ui/scroll_up"
```

**After:**
```
key event → InputStore resolves binding → dispatches write to "command/invoke"
          with CommandInvocation { command: "scroll_up", args: {} }
          → CommandStore validates → dispatches write to "ui/scroll_up"
```

The `Action` enum gains an `Invoke` variant:

```rust
pub enum Action {
    /// Legacy: raw path + fields (deprecated, will be removed)
    Command {
        target: Path,
        fields: Vec<ActionField>,
    },
    /// Command invocation through the registry
    Invoke {
        command: String,
        args: BTreeMap<String, Value>,
    },
    /// Sequence of actions
    Macro(Vec<Action>),
}
```

For `Action::Invoke`, the InputStore builds a `CommandInvocation` and
writes it to `command/invoke`. For `Action::Command`, the existing
direct-dispatch path remains until migration is complete.

### Help / shortcuts discovery

A help screen or shortcuts view reads from the CommandStore to build
its display:

```rust
// Read all user-facing commands
let commands = client.read(&path!("command/commands")).await;

// Read bindings for current mode
let bindings = client.read(&path!("input/bindings/normal")).await;

// Correlate: for each binding, find its command's description
```

Because `CommandDef` is `Deserialize`, the read result can be
deserialized back into typed Rust structs for rendering logic. No
string parsing, no ad-hoc value inspection.

The shortcuts help screen becomes data-driven: it reads the command
catalog and binding table, correlates them, and renders. Adding a new
command or rebinding a key automatically updates the help.

### Error handling

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandError {
    /// Command name not found in registry
    UnknownCommand { name: String },
    /// Required parameter missing
    MissingParam { command: String, param: String },
    /// Parameter value has wrong type
    TypeMismatch {
        command: String,
        param: String,
        expected: String,
        got: String,
    },
    /// Enum parameter has invalid value
    InvalidValue {
        command: String,
        param: String,
        allowed: Vec<String>,
        got: String,
    },
    /// Command name already registered
    DuplicateName { name: String },
}
```

`CommandError` is serializable so it can be returned through store
write failures and displayed by any shell.

### Built-in command catalog

Every action currently handled by ox stores gets a `CommandDef`. The
complete catalog:

#### Navigation

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `select_next` | `ui/select_next` | — | Move selection down |
| `select_prev` | `ui/select_prev` | — | Move selection up |
| `select_first` | `ui/select_first` | — | Jump to first item |
| `select_last` | `ui/select_last` | — | Jump to last item |
| `scroll_up` | `ui/scroll_up` | — | Scroll viewport up |
| `scroll_down` | `ui/scroll_down` | — | Scroll viewport down |
| `scroll_to_top` | `ui/scroll_to_top` | — | Scroll to top |
| `scroll_to_bottom` | `ui/scroll_to_bottom` | — | Scroll to bottom |
| `scroll_page_up` | `ui/scroll_page_up` | — | Scroll one page up |
| `scroll_page_down` | `ui/scroll_page_down` | — | Scroll one page down |
| `scroll_half_page_up` | `ui/scroll_half_page_up` | — | Scroll half page up |
| `scroll_half_page_down` | `ui/scroll_half_page_down` | — | Scroll half page down |

#### Screen transitions

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `open` | `ui/open` | `thread_id: String` (required) | Open a thread |
| `close` | `ui/close` | — | Back to inbox |
| `settings` | `ui/go_to_settings` | — | Open settings screen |
| `inbox` | `ui/go_to_inbox` | — | Return to inbox |
| `open_selected` | `ui/open_selected` | — | Open currently selected thread |
| `quit` | `ui/quit` | — | Quit the application |

#### Mode transitions

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `compose` | `ui/enter_insert` | `context: Enum(compose, reply, search)` (required, default `"compose"`) | Open compose input |
| `reply` | `ui/enter_insert` | `context: Enum(compose, reply, search)` (required, default `"reply"`) | Open reply input |
| `search` | `ui/enter_insert` | `context: Enum(compose, reply, search)` (required, default `"search"`) | Open search input |
| `exit_insert` | `ui/exit_insert` | — | Exit insert mode |

Note: `compose`, `reply`, and `search` are user-facing aliases for
`enter_insert` with pre-bound context. The underlying store command is
the same. This is intentional — the command vocabulary is for humans,
the store paths are for the protocol.

#### Text input

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `send_input` | `ui/send_input` | — | Send current input |
| `clear_input` | `ui/clear_input` | — | Clear input buffer |

#### Thread actions

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `archive_selected` | `ui/archive_selected` | — | Archive selected thread |

#### Search

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `search_insert_char` | `ui/search_insert_char` | `char: String` (required) | Append to search query |
| `search_delete_char` | `ui/search_delete_char` | — | Delete last search char |
| `search_clear` | `ui/search_clear` | — | Clear search query |
| `search_save_chip` | `ui/search_save_chip` | — | Save query as search chip |
| `search_dismiss_chip` | `ui/search_dismiss_chip` | `index: Integer` (required) | Remove a search chip |

#### Modals

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `show_modal` | `ui/show_modal` | `modal: Map` (required) | Show a modal dialog |
| `dismiss_modal` | `ui/dismiss_modal` | — | Dismiss current modal |

#### Approval

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `approve` | `approval/response` | `decision: Enum(allow_once, deny_once, allow_session, allow_always, deny_always)` (required) | Respond to approval request |

#### Internal (not user-facing)

These commands are used by the system to sync state. They are registered
in the catalog for completeness but `user_facing: false` hides them
from commandline help.

| Command | Target | Params | Description |
|---------|--------|--------|-------------|
| `set_row_count` | `ui/set_row_count` | `count: Integer` (required) | Set list row count |
| `set_scroll_max` | `ui/set_scroll_max` | `max: Integer` (required) | Set max scroll position |
| `set_viewport_height` | `ui/set_viewport_height` | `height: Integer` (required) | Set viewport height |
| `set_input` | `ui/set_input` | `text: String`, `cursor: Integer` | Set input content |
| `set_status` | `ui/set_status` | `text: String` | Set status bar message |
| `clear_pending_action` | `ui/clear_pending_action` | — | Clear pending action flag |

### Crate placement

All types live in **ox-ui**:

- `CommandDef`, `ParamDef`, `ParamKind` — serializable definition types
- `StaticCommandDef`, `StaticParamDef`, `StaticParamKind` — `&'static` helpers
- `CommandInvocation` — serializable invocation type
- `CommandRegistry` — plain Rust lookup + validation (no StructFS dep)
- `CommandStore` — StructFS Reader/Writer wrapper around `CommandRegistry`
- `CommandError` — serializable error type
- `builtin_commands()` — static catalog of all built-in commands

ox-ui is already platform-agnostic. The command types add `serde` as a
dependency (already present in ox-ui's dependency tree via
`structfs-serde-store`).

### Keybinding changes

As part of this work, the default keybindings change:

| Old | New | Action |
|-----|-----|--------|
| `i` (inbox) | `c` (inbox) | Compose new thread |
| `i` (thread) | `c` (thread) | Reply in thread |
| `Esc` (insert) | `Ctrl+q` (insert) | Exit insert mode |
| — | `:` (normal) | Enter commandline |
| — | `;` (normal) | Enter commandline (alias) |

### Broker mount

The CommandStore is mounted at `command/` in the broker namespace:

```rust
// In broker_setup
servers.push(broker.mount(path!("command"), command_store).await);
```

This makes it accessible at:
- `command/commands` — read the catalog
- `command/invoke` — execute a command
- `command/register` — add a command

## What this RFC does NOT cover

- **Commandline rendering.** The visual appearance of the `:` input line
  is an implementation detail of each shell.

- **Tab completion.** The registry provides the data (command names, param
  names, enum values) but completion UX is shell-specific.

- **Scripting language.** This RFC defines the command invocation interface
  that a scripting language would target. The scripting language itself is
  a separate design.

- **Command mode as a UiStore mode.** Whether "command" becomes a third
  `Mode` variant (alongside Normal/Insert) is an implementation decision,
  not a protocol decision.

## Migration

Existing `Action::Command { target, fields }` bindings continue to work.
New bindings should use `Action::Invoke { command, args }`. The old variant
can be removed once all bindings are migrated.

The migration is mechanical: for each binding, find the `CommandDef` whose
target matches, replace the `Action::Command` with `Action::Invoke` using
the command's name and mapped args.
