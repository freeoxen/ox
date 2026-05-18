# horns UI toolkit extraction — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the path-MVU UI framework currently living in `ox-view`, parts of `ox-types`, and `ox-cli/src/settings/` into three new crates — `horns-core`, `horns-ratatui`, and `horns` — with a StructFS-only interface (mount on broker, no function calls into horns after install).

**Architecture:** Three crates. `horns-core` carries the framework primitives (View enum, registries, dispatcher, subscriptions, install API) and depends only on `structfs-core-store` and `ox-broker`. `horns-ratatui` is the ratatui backend mount. `horns` is the umbrella with a `ratatui` feature. Screen and Mode discriminators drop from the framework; the cursor path is configured at install time. A `KeyHandler` tier sits next to the discrete `BindingEntry` tier for opaque event consumers; the settings `_edit` scope migrates from 96 BindingEntries to one TextInputHandler.

**Tech Stack:** Rust 2024, structfs, ratatui 0.29, crossterm 0.28, tokio, insta for snapshot tests, the existing ox-broker DispatchingStore.

**Reference spec:** `docs/superpowers/specs/2026-05-15-horns-ui-toolkit-extraction-design.md`. Read it before starting any task — every task here trusts the spec for context.

---

## Conventions used throughout this plan

- **Path prefixes:** Paths starting with `crates/horns-core/...` are inside the new horns-core crate; `crates/ox-cli/...` is the existing CLI crate, etc.
- **Verification command:** `cargo build -p <crate>` and `cargo test -p <crate>` are run from the workspace root (`/Users/alex/Devel/AdjectiveNoun/ox/`).
- **Commit messages:** present-tense, ≤72 char subject, body when explaining *why*. Match the recent style (`fix:`, `feat:`, `refactor:`, `spec:`, `tweak:`, no Co-Authored-By trailer).
- **Don't use worktrees** — edits land directly in the main checkout (per repo convention).
- **TDD where it pays:** new functionality (KeyHandler, install API, subscriptions, _edit migration) gets a failing test first. File moves don't need new tests; the existing tests come along and continue to pass.

---

## Task 1: Workspace scaffolding — create the three empty crates

**Files:**
- Create: `crates/horns-core/Cargo.toml`
- Create: `crates/horns-core/src/lib.rs`
- Create: `crates/horns-ratatui/Cargo.toml`
- Create: `crates/horns-ratatui/src/lib.rs`
- Create: `crates/horns/Cargo.toml`
- Create: `crates/horns/src/lib.rs`
- Modify: `Cargo.toml` (workspace `members` and `default-members`)

- [ ] **Step 1: Create `crates/horns-core/Cargo.toml`**

```toml
[package]
name = "horns-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true
description = "Path-MVU UI framework primitives — View enum, dispatcher, registries, broker-mount install API"
publish = false

[dependencies]
structfs-core-store = { workspace = true }
ox-broker = { path = "../ox-broker" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = { workspace = true }

[dev-dependencies]
tokio = { version = "1", features = ["sync", "time", "rt", "rt-multi-thread", "macros"] }
```

- [ ] **Step 2: Create `crates/horns-core/src/lib.rs`**

```rust
//! horns-core: path-MVU UI framework primitives.
//!
//! See `crates/horns/docs/` for the full reader documentation.
```

- [ ] **Step 3: Create `crates/horns-ratatui/Cargo.toml`**

```toml
[package]
name = "horns-ratatui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true
description = "Ratatui backend mount for horns — translates the View tree to terminal frames"
publish = false

[dependencies]
horns-core = { path = "../horns-core" }
ox-broker = { path = "../ox-broker" }
ratatui = "0.29"
unicode-width = "0.2"
structfs-core-store = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
insta = "1"
tokio = { version = "1", features = ["sync", "time", "rt", "rt-multi-thread", "macros"] }
```

- [ ] **Step 4: Create `crates/horns-ratatui/src/lib.rs`**

```rust
//! horns-ratatui: ratatui backend for the horns framework.
//!
//! Installs a subscription that watches the configured view-input path
//! and draws each new View to a ratatui Terminal.
```

- [ ] **Step 5: Create `crates/horns/Cargo.toml`**

```toml
[package]
name = "horns"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true
description = "Path-MVU UI toolkit — umbrella crate"
publish = false

[features]
default = ["ratatui"]
ratatui = ["dep:horns-ratatui"]

[dependencies]
horns-core = { path = "../horns-core" }
horns-ratatui = { path = "../horns-ratatui", optional = true }
```

- [ ] **Step 6: Create `crates/horns/src/lib.rs`**

```rust
//! horns: path-MVU UI toolkit.
//!
//! Re-exports horns-core. With the `ratatui` feature (on by default)
//! also exposes `horns::ratatui` re-exporting horns-ratatui.

pub use horns_core::*;

#[cfg(feature = "ratatui")]
pub mod ratatui {
    pub use horns_ratatui::*;
}
```

- [ ] **Step 7: Wire all three crates into the workspace**

In `/Users/alex/Devel/AdjectiveNoun/ox/Cargo.toml`, inside both `members` and `default-members` arrays, add the three new crate paths after `crates/ox-view`:

```toml
"crates/horns-core",
"crates/horns-ratatui",
"crates/horns",
```

- [ ] **Step 8: Verify the new crates build**

Run: `cargo build -p horns-core -p horns-ratatui -p horns`
Expected: clean build.

- [ ] **Step 9: Verify the existing workspace still builds**

Run: `cargo build`
Expected: clean build. No regressions.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml crates/horns-core crates/horns-ratatui crates/horns
git commit -m "feat: scaffold horns-core, horns-ratatui, horns crates"
```

---

## Task 2: Move View enum from ox-view into horns-core (with serde derives)

**Files:**
- Create: `crates/horns-core/src/view.rs`
- Create: `crates/horns-core/src/path_serde.rs` (copied from `ox-types/src/path_serde.rs`)
- Modify: `crates/horns-core/src/lib.rs`
- Delete: `crates/ox-view/` (entire crate)
- Modify: `Cargo.toml` (drop `crates/ox-view` from members + default-members)
- Modify: `crates/ox-cli/Cargo.toml` (replace `ox-view` dep with `horns-core`)
- Modify: every `ox-cli` file that imports `ox_view::*`

- [ ] **Step 1: Copy ox-view's contents into horns-core**

```bash
cp /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-view/src/lib.rs \
   /Users/alex/Devel/AdjectiveNoun/ox/crates/horns-core/src/view.rs
cp /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-types/src/path_serde.rs \
   /Users/alex/Devel/AdjectiveNoun/ox/crates/horns-core/src/path_serde.rs
```

- [ ] **Step 2: Add serde derives to View and supporting types**

The View tree must serialize to ride a broker path. In `crates/horns-core/src/view.rs`, add `#[derive(Serialize, Deserialize)]` to every public type that already derives `Debug, Clone, PartialEq`:

`View`, `Span`, `ListItem`, `FormRow`, `FormValue`, `BannerKind`, `StyledLine`, `Direction`, `Sizing`, `Padding`, `Align`, `Style`, `Color`, `ModifierSet`, `FocusId` if present.

At the top of the file add:

```rust
use serde::{Deserialize, Serialize};
```

For fields wrapping `structfs_core_store::Path` (e.g., `FocusId(Path)`), annotate with the local `path_serde` module: `#[serde(with = "crate::path_serde")]`. `Box<View>` serializes transparently.

- [ ] **Step 3: Wire modules into horns-core**

In `crates/horns-core/src/lib.rs`:

```rust
//! horns-core: path-MVU UI framework primitives.

pub mod view;
pub(crate) mod path_serde;

pub use view::View;
```

- [ ] **Step 4: Verify horns-core builds**

Run: `cargo build -p horns-core`
Expected: clean build.

- [ ] **Step 5: Add a serde round-trip smoke test**

Append to `crates/horns-core/src/view.rs`:

```rust
#[cfg(test)]
mod serde_smoke {
    use super::*;

    #[test]
    fn view_round_trips_through_json() {
        let v = View::Text {
            spans: vec![Span::plain("hello")],
            align: Align::Left,
        };
        let s = serde_json::to_string(&v).unwrap();
        let back: View = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn list_view_with_nested_items_round_trips() {
        let v = View::List {
            items: vec![ListItem {
                primary: "one".into(),
                primary_spans: None,
                secondary: None,
                badge: None,
                focus: None,
            }],
            selected: Some(0),
        };
        let s = serde_json::to_string(&v).unwrap();
        let back: View = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back);
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p horns-core`
Expected: 2 tests pass. Compile errors mean a serde derive is missing somewhere in the type tree; chase it.

- [ ] **Step 7: Replace ox-view dep with horns-core in ox-cli**

In `crates/ox-cli/Cargo.toml`, replace `ox-view = { path = "../ox-view" }` with `horns-core = { path = "../horns-core" }` (or confirm it's already present from Task 1).

- [ ] **Step 8: Update every ox-cli file that imports ox_view**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
grep -rl "ox_view::" crates/ox-cli/src/ | xargs sed -i.bak 's|ox_view::|horns_core::view::|g'
grep -rl "use ox_view" crates/ox-cli/src/ | xargs sed -i.bak 's|use ox_view\b|use horns_core::view|g'
find crates/ox-cli/src -name "*.bak" -delete
```

Verify visually with `git diff crates/ox-cli/` before proceeding.

- [ ] **Step 9: Verify ox-cli builds**

Run: `cargo build -p ox-cli`
Expected: clean build. Any unresolved import means a sed pattern missed — fix it.

- [ ] **Step 10: Delete ox-view from the workspace**

```bash
rm -rf /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-view
```

Remove `"crates/ox-view"` from both `members` and `default-members` in the workspace `Cargo.toml`.

- [ ] **Step 11: Verify full workspace build and test**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
refactor: fold ox-view into horns-core::view

View enum and supporting types move verbatim into horns-core/src/view.rs.
Adds serde derives so the View tree can ride a broker path (needed for
the render-output subscription in Task 8). ox-view crate is deleted;
ox-cli imports horns_core::view.
CM
)"
```

---

## Task 3: Move KeyChord and binding/command data types from ox-types into horns-core

**Files:**
- Create: `crates/horns-core/src/key.rs` (from `ox-types/src/key_chord.rs`)
- Create: `crates/horns-core/src/binding.rs` (data half: `BindingScope`, `Phase`, `BindingId`)
- Create: `crates/horns-core/src/command.rs` (data half: `CommandId`, `CommandDisplay`, `CommandScope` minus `Screen`)
- Create: `crates/horns-core/src/write.rs` (`Write` type from `ox-types/src/subscription.rs`)
- Modify: `crates/horns-core/src/lib.rs`
- Modify: `crates/ox-types/src/key_chord.rs` (becomes a re-export shim)
- Modify: `crates/ox-types/src/command_binding.rs` (becomes a re-export shim, plus a temporary local `BindingEntry` that the registry move in Task 4 deletes)
- Modify: `crates/ox-types/src/lib.rs`
- Modify: `crates/ox-types/Cargo.toml` (add `horns-core` dep)
- Modify: every ox-cli construction of `BindingEntry { screen, mode, ... }` or `CommandScope { screen, ... }` to drop those fields

**Strategy:** ox-types keeps re-export shims so callers compile through Tasks 3–7 unchanged. The big import sweep happens in Task 10.

- [ ] **Step 1: Move `ox-types/src/key_chord.rs` to `horns-core/src/key.rs`**

```bash
cp /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-types/src/key_chord.rs \
   /Users/alex/Devel/AdjectiveNoun/ox/crates/horns-core/src/key.rs
```

Open the new file and replace the deferred-`from_crossterm` doc comment that references "settings-screen redesign §C2" with:

```rust
//! Backend-agnostic key chord representation.
//!
//! `KeyChord` is the dispatch input type. Hosts convert their backend's
//! native key events (crossterm KeyEvent, browser KeyboardEvent, etc.)
//! into KeyChord and write it to horns' configured `<input_path>/key`.
```

- [ ] **Step 2: Split `command_binding.rs` into `binding.rs` and `command.rs`**

Read `crates/ox-types/src/command_binding.rs`. Create `crates/horns-core/src/binding.rs` carrying just `BindingScope`, `Phase`, plus a new `BindingId`:

```rust
//! Binding data shapes. The full registry impl lives in this file
//! after Task 4 moves it from ox-cli/src/settings/binding_registry.rs.

use serde::{Deserialize, Serialize};
use structfs_core_store::Path;

use crate::path_serde;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingScope {
    Anywhere,
    Exact(#[serde(with = "path_serde")] Path),
    Prefix(#[serde(with = "path_serde")] Path),
}

impl BindingScope {
    pub fn matches(&self, cursor: &Path) -> bool {
        match self {
            BindingScope::Anywhere => true,
            BindingScope::Exact(p) => p.components == cursor.components,
            BindingScope::Prefix(p) => {
                p.components.len() <= cursor.components.len()
                    && cursor.components[..p.components.len()] == p.components[..]
            }
        }
    }

    pub fn keyed_path(&self) -> Option<&Path> {
        match self {
            BindingScope::Anywhere => None,
            BindingScope::Exact(p) | BindingScope::Prefix(p) => Some(p),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase { Capture, Target, Bubble }

/// Stable identifier for a registered binding. Used as the path
/// component under `<bindings_prefix>/<binding-id>`.
#[derive(Hash, Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingId(pub String);
```

Create `crates/horns-core/src/command.rs` carrying `CommandId`, `CommandDisplay`, `CommandScope` — with `CommandScope` dropping the `screen` field:

```rust
//! Command data shapes. The Command trait and registry impl land in
//! this file after Task 4 moves them from ox-cli.

use serde::{Deserialize, Serialize};
use structfs_core_store::Path;

use crate::path_serde;

#[derive(Hash, Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDisplay {
    pub name: String,
    pub description: String,
}

/// The cursor scope a command applies to. `cursor_path = None` means
/// screen-wide. (Screen itself is no longer a framework concept; hosts
/// install one horns instance per screen at disjoint prefixes.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandScope {
    #[serde(with = "path_serde::option", default)]
    pub cursor_path: Option<Path>,
}
```

- [ ] **Step 3: Create `crates/horns-core/src/write.rs`**

Read the existing `Write` definition in `crates/ox-types/src/subscription.rs`. Copy into:

```rust
//! A pending write into the broker: (path, record).

use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Record};

use crate::path_serde;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Write {
    #[serde(with = "path_serde")]
    pub path: Path,
    pub record: Record,
}
```

If `Record` already implements `Serialize`/`Deserialize` (check ox-broker source), this builds; if not, use the same `path_serde`-style adapter or rely on ox-broker's existing helpers (read `crates/ox-types/src/subscription.rs` for the current shape).

- [ ] **Step 4: Wire the new modules into horns-core**

In `crates/horns-core/src/lib.rs`:

```rust
pub mod view;
pub mod key;
pub mod binding;
pub mod command;
pub mod write;
pub(crate) mod path_serde;

pub use binding::{BindingId, BindingScope, Phase};
pub use command::{CommandDisplay, CommandId, CommandScope};
pub use key::{KeyChord, KeyCodeRepr, KeyModifierSet};
pub use view::View;
pub use write::Write;
```

- [ ] **Step 5: Verify horns-core builds**

Run: `cargo build -p horns-core`
Expected: clean build.

- [ ] **Step 6: Add `horns-core` as a dep of ox-types**

In `crates/ox-types/Cargo.toml`:

```toml
[dependencies]
# existing entries...
horns-core = { path = "../horns-core" }
```

- [ ] **Step 7: Replace `ox-types/src/key_chord.rs` with a re-export shim**

```rust
//! Compatibility shim: the data types live in horns-core now.
//! Remove when all callers import from horns_core::key directly.

pub use horns_core::key::{KeyChord, KeyCodeRepr, KeyModifierSet};
```

- [ ] **Step 8: Replace `ox-types/src/command_binding.rs` with a shim**

The shim keeps `BindingEntry` definition local (Task 4 moves it). Drop `screen` and `mode` fields from `BindingEntry`:

```rust
//! Compatibility shim while the binding registry is still in ox-cli.
//! Task 4 moves BindingEntry into horns-core/src/binding.rs; this
//! file disappears then.

use serde::{Deserialize, Serialize};

pub use horns_core::binding::{BindingId, BindingScope, Phase};
pub use horns_core::command::{CommandDisplay, CommandId, CommandScope};
pub use horns_core::key::KeyChord;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    pub scope: BindingScope,
    pub key: KeyChord,
    pub phase: Phase,
    pub command_id: CommandId,
}
```

- [ ] **Step 9: Update `ox-types/src/lib.rs` re-exports**

Ensure these `pub use` lines work after the file edits:

```rust
pub use command_binding::{BindingEntry, BindingId, BindingScope, CommandDisplay, CommandId, CommandScope, Phase};
pub use key_chord::{KeyChord, KeyCodeRepr, KeyModifierSet};
```

- [ ] **Step 10: Strip `screen:`/`mode:` from every ox-cli `BindingEntry` and `CommandScope` constructor**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
grep -rn "BindingEntry {" crates/ox-cli/src/ | head -50
grep -rn "CommandScope {" crates/ox-cli/src/ | head -20
```

For each match, remove the `screen: Screen::...` and `mode: Some(Mode::...)` fields. There are ~30+ `BindingEntry` constructors in `crates/ox-cli/src/settings/bindings.rs`; this is mechanical. Where `Mode::Insert`-specific bindings existed, drop the `mode:` field — those bindings are about to be replaced by the TextInputHandler in Task 9 anyway.

- [ ] **Step 11: Build the full workspace**

Run: `cargo build`
Expected: clean build. Compile errors point at the missing fields and missing imports — fix each in place.

- [ ] **Step 12: Run the full test suite**

Run: `cargo test`
Expected: every test passes. Test fixtures setting `mode:` need the same field drop.

- [ ] **Step 13: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
refactor: move KeyChord and binding/command data types into horns-core

Data shapes (KeyChord, BindingScope, Phase, CommandId, CommandDisplay,
CommandScope, Write) move from ox-types into horns-core. Screen and Mode
discriminators drop from BindingEntry and CommandScope — these are
application concerns, not framework concerns. ox-types keeps temporary
re-export shims so callers compile unchanged through the rest of the
extraction.
CM
)"
```

---

## Task 4: Move the three registries into horns-core

**Files:**
- Modify: `crates/horns-core/src/binding.rs` — add `BindingEntry`, `BindingRegistry`
- Modify: `crates/horns-core/src/command.rs` — add `Command` trait, `CommandRegistry`, `CommandCtx`, `CommandMetadata`
- Create: `crates/horns-core/src/render.rs` — `Renderer` trait, `RendererRegistry`, `RenderCtx`, `AscendRule`, `RendererMetadata`
- Delete: `crates/ox-cli/src/settings/binding_registry.rs`
- Delete: `crates/ox-cli/src/settings/command_registry.rs`
- Delete: `crates/ox-cli/src/settings/registry.rs`
- Delete: `crates/ox-types/src/command_binding.rs` (entirely; the shim from Task 3 goes away)
- Modify: `crates/ox-cli/src/settings/mod.rs` — remove `pub mod binding_registry/command_registry/registry`
- Modify: ox-cli imports throughout

- [ ] **Step 1: Move the BindingRegistry implementation into `crates/horns-core/src/binding.rs`**

Read the existing impl in `crates/ox-cli/src/settings/binding_registry.rs`. Re-implement in horns-core with the discrete tier only (handler tier comes in Task 7); strip `screen` and `mode` from the lookup:

```rust
use crate::command::CommandId;
use crate::key::KeyChord;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    pub scope: BindingScope,
    pub key: KeyChord,
    pub phase: Phase,
    pub command_id: CommandId,
}

pub struct BindingRegistry {
    entries: Vec<BindingEntry>,
}

impl BindingRegistry {
    pub fn new() -> Self { Self { entries: Vec::new() } }
    pub fn register(&mut self, entry: BindingEntry) { self.entries.push(entry); }
    pub fn entries(&self) -> &[BindingEntry] { &self.entries }

    /// Specificity: Exact > Prefix(deeper) > Prefix(shallower) > Anywhere.
    /// Ties broken by registration order.
    pub fn lookup(
        &self,
        cursor: &structfs_core_store::Path,
        key: &KeyChord,
        phase: Phase,
    ) -> Option<&CommandId> {
        let mut best: Option<(usize, usize, &BindingEntry)> = None;
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.phase != phase { continue; }
            if entry.key != *key { continue; }
            if !entry.scope.matches(cursor) { continue; }
            let tier = match &entry.scope {
                BindingScope::Exact(_) => 3_000_000,
                BindingScope::Prefix(p) => 2_000_000 + p.components.len(),
                BindingScope::Anywhere => 1_000_000,
            };
            // Prefer higher tier; on tie, prefer earlier registration.
            let candidate = (tier, usize::MAX - idx);
            if best.map_or(true, |(t, o, _)| candidate > (t, o)) {
                best = Some((candidate.0, candidate.1, entry));
            }
        }
        best.map(|(_, _, e)| &e.command_id)
    }
}

impl Default for BindingRegistry {
    fn default() -> Self { Self::new() }
}
```

Copy existing unit tests from `crates/ox-cli/src/settings/binding_registry.rs` into a `#[cfg(test)] mod tests { ... }` at the bottom. Strip `screen:`/`mode:` from every test fixture as you copy.

- [ ] **Step 2: Move the CommandRegistry into `crates/horns-core/src/command.rs`**

Read `crates/ox-cli/src/settings/command_registry.rs`. The current `Command` trait references the in-crate `RendererRegistry`. Move both together; `CommandCtx` carries `&RendererRegistry` so commands can resolve `AscendRule`.

```rust
use std::collections::HashMap;
use structfs_core_store::Reader;

use crate::key::KeyChord;
use crate::write::Write;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandMetadata {
    pub display: CommandDisplay,
    pub scope: CommandScope,
}

pub struct CommandCtx<'a> {
    pub last_keystroke: Option<KeyChord>,
    pub renderers: &'a crate::render::RendererRegistry,
}

pub trait Command: Send + Sync {
    fn id(&self) -> &CommandId;
    fn display(&self) -> &CommandDisplay;
    fn scope(&self) -> &CommandScope;
    fn run(&self, snapshot: &mut dyn Reader, ctx: &CommandCtx<'_>) -> Vec<Write>;
}

pub struct CommandRegistry {
    by_id: HashMap<CommandId, Box<dyn Command>>,
}

impl CommandRegistry {
    pub fn new() -> Self { Self { by_id: HashMap::new() } }
    pub fn register(&mut self, command: Box<dyn Command>) {
        self.by_id.insert(command.id().clone(), command);
    }
    pub fn lookup(&self, id: &CommandId) -> Option<&dyn Command> {
        self.by_id.get(id).map(|b| &**b)
    }
    pub fn iter(&self) -> impl Iterator<Item = &dyn Command> {
        self.by_id.values().map(|b| &**b)
    }
}

impl Default for CommandRegistry {
    fn default() -> Self { Self::new() }
}
```

Copy unit tests from the old file.

- [ ] **Step 3: Move the RendererRegistry into `crates/horns-core/src/render.rs`**

Read `crates/ox-cli/src/settings/registry.rs`. Create:

```rust
//! Renderer registry: cursor path -> Box<dyn Renderer>.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Rect, Reader};

use crate::view::View;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AscendRule {
    NearestRegistered,
    ExitScreen,
}

pub struct RenderCtx<'a> {
    pub area: Rect,
    pub data: &'a mut dyn Reader,
    pub registry: &'a RendererRegistry,
    /// Theme is host-defined; horns-core has no concrete Theme type.
    /// Backends downcast or read theme from the broker via the snapshot.
    pub theme: &'a dyn std::any::Any,
}

pub trait Renderer: Send + Sync {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View;
    fn ascend_to(&self) -> AscendRule;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererMetadata {
    pub ascend_rule: AscendRule,
}

pub struct RendererRegistry {
    specs: HashMap<Path, Box<dyn Renderer>>,
}

impl RendererRegistry {
    pub fn new() -> Self { Self { specs: HashMap::new() } }
    pub fn register(&mut self, cursor: Path, renderer: Box<dyn Renderer>) {
        self.specs.insert(cursor, renderer);
    }
    pub fn lookup(&self, cursor: &Path) -> Option<&dyn Renderer> {
        self.specs.get(cursor).map(|b| &**b)
    }
    pub fn render(&self, cursor: &Path, ctx: &mut RenderCtx<'_>) -> View {
        match self.specs.get(cursor) {
            Some(r) => r.render(ctx),
            None => View::unknown_cursor_fallback(cursor),
        }
    }
    pub fn ascend(&self, cursor: &Path) -> Option<Path> {
        let r = self.specs.get(cursor)?;
        match r.ascend_to() {
            AscendRule::ExitScreen => None,
            AscendRule::NearestRegistered => {
                let mut p = cursor.parent()?;
                loop {
                    if self.specs.contains_key(&p) { return Some(p); }
                    p = p.parent()?;
                }
            }
        }
    }
}

impl Default for RendererRegistry {
    fn default() -> Self { Self::new() }
}
```

**Note on theme:** `RenderCtx::theme` is `&'a dyn std::any::Any` because Theme lives in `horns-ratatui`. ox-cli renderers downcast at use site. If this proves painful in Task 9, revisit by routing theme through the broker snapshot instead.

Copy tests from the old file.

- [ ] **Step 4: Wire into `crates/horns-core/src/lib.rs`**

```rust
pub mod view;
pub mod key;
pub mod binding;
pub mod command;
pub mod render;
pub mod write;
pub(crate) mod path_serde;

pub use binding::{BindingEntry, BindingId, BindingRegistry, BindingScope, Phase};
pub use command::{Command, CommandCtx, CommandDisplay, CommandId, CommandMetadata, CommandRegistry, CommandScope};
pub use key::{KeyChord, KeyCodeRepr, KeyModifierSet};
pub use render::{AscendRule, Renderer, RenderCtx, RendererMetadata, RendererRegistry};
pub use view::View;
pub use write::Write;
```

- [ ] **Step 5: Build and test horns-core**

Run: `cargo build -p horns-core && cargo test -p horns-core`
Expected: clean build, migrated tests pass.

- [ ] **Step 6: Delete the moved files from ox-cli and ox-types**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
rm crates/ox-cli/src/settings/binding_registry.rs
rm crates/ox-cli/src/settings/command_registry.rs
rm crates/ox-cli/src/settings/registry.rs
rm crates/ox-types/src/command_binding.rs
```

In `crates/ox-cli/src/settings/mod.rs`, remove the lines:
```rust
pub mod binding_registry;
pub mod command_registry;
pub mod registry;
```

Add (or update) re-exports:
```rust
pub use horns_core::{
    AscendRule, BindingEntry, BindingRegistry, Command, CommandCtx, CommandId,
    CommandRegistry, RenderCtx, Renderer, RendererRegistry,
};
```

In `crates/ox-types/src/lib.rs`, replace `pub mod command_binding;` with re-exports from horns_core:
```rust
pub use horns_core::{
    BindingEntry, BindingId, BindingScope, CommandDisplay, CommandId, CommandScope, Phase,
};
```

- [ ] **Step 7: Update remaining super-imports**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
grep -rln "super::binding_registry\|super::command_registry\|super::registry::\|settings::binding_registry\|settings::command_registry\|settings::registry::" crates/ox-cli/src/
```

For each match, route through `horns_core::` instead. Visually review with `git diff` before continuing.

- [ ] **Step 8: Build and test the workspace**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
refactor: move BindingRegistry, CommandRegistry, RendererRegistry to horns-core

The three framework registries leave ox-cli/src/settings/. Screen and Mode
are gone from BindingRegistry::lookup — Screen is now an application concern
(hosts install one horns instance per screen at disjoint prefixes); Mode is
encoded in cursor path segments under the cursor-as-focus model.

RenderCtx::theme becomes &dyn Any so horns-core stays Theme-agnostic;
backends downcast. ox-cli renderers may revisit this if it proves painful.
CM
)"
```

---

## Task 5: Move the dispatcher into horns-core

**Files:**
- Create: `crates/horns-core/src/dispatch.rs`
- Modify: `crates/horns-core/src/lib.rs`
- Delete: `crates/ox-cli/src/settings/dispatch.rs`
- Modify: `crates/ox-cli/src/dispatch.rs` (top-level legacy `send_key` — bridge to `Dispatcher` during the transition; deleted in Task 10)

- [ ] **Step 1: Create `crates/horns-core/src/dispatch.rs`**

```rust
//! Hierarchical key dispatch over the cursor's ancestor chain.

use structfs_core_store::{Path, Reader};

use crate::binding::{BindingRegistry, BindingScope, Phase};
use crate::command::{CommandCtx, CommandId, CommandRegistry};
use crate::key::KeyChord;
use crate::render::RendererRegistry;
use crate::write::Write;

pub struct Dispatcher {
    cursor_path: Path,
}

impl Dispatcher {
    pub fn new(cursor_path: Path) -> Self {
        Self { cursor_path }
    }

    pub fn cursor_path(&self) -> &Path { &self.cursor_path }

    /// Capture → Target → Bubble over the cursor's ancestor chain.
    /// Inert (empty Vec) on no match.
    pub fn dispatch(
        &self,
        snapshot: &mut dyn Reader,
        key: &KeyChord,
        bindings: &BindingRegistry,
        commands: &CommandRegistry,
        renderers: &RendererRegistry,
    ) -> Vec<Write> {
        let scope_path = self.compute_scope_path(snapshot);

        // Capture (outer → inner).
        for scope in &scope_path {
            if let Some(p) = scope.keyed_path() {
                if let Some(id) = bindings.lookup(p, key, Phase::Capture) {
                    return run(commands, renderers, snapshot, key, id);
                }
            }
        }

        // Target (leaf only).
        if let Some(leaf) = scope_path.last().and_then(BindingScope::keyed_path) {
            if let Some(id) = bindings.lookup(leaf, key, Phase::Target) {
                return run(commands, renderers, snapshot, key, id);
            }
        }

        // Bubble (inner → outer).
        for scope in scope_path.iter().rev() {
            if let Some(p) = scope.keyed_path() {
                if let Some(id) = bindings.lookup(p, key, Phase::Bubble) {
                    return run(commands, renderers, snapshot, key, id);
                }
            }
        }

        vec![]
    }

    fn compute_scope_path(&self, snapshot: &mut dyn Reader) -> Vec<BindingScope> {
        let Some(cursor) = read_focus_cursor(snapshot, &self.cursor_path) else {
            return Vec::new();
        };
        path_ancestors(&cursor).into_iter().map(BindingScope::Exact).collect()
    }
}

fn run(
    commands: &CommandRegistry,
    renderers: &RendererRegistry,
    snapshot: &mut dyn Reader,
    key: &KeyChord,
    id: &CommandId,
) -> Vec<Write> {
    let Some(cmd) = commands.lookup(id) else { return vec![]; };
    let ctx = CommandCtx {
        last_keystroke: Some(key.clone()),
        renderers,
    };
    cmd.run(snapshot, &ctx)
}

fn read_focus_cursor(snapshot: &mut dyn Reader, cursor_path: &Path) -> Option<Path> {
    let rec = snapshot.read(cursor_path).ok().flatten()?;
    let value = rec.as_value()?;
    let s = value.as_str()?;
    Path::parse(s).ok()
}

/// Returns `[root, ..., p]` — the cursor's full ancestor chain, outer→inner.
pub fn path_ancestors(p: &Path) -> Vec<Path> {
    let mut acc = Vec::with_capacity(p.components.len());
    for i in 1..=p.components.len() {
        acc.push(Path { components: p.components[..i].to_vec() });
    }
    acc
}
```

Copy dispatch tests from `crates/ox-cli/src/settings/dispatch.rs` into a `#[cfg(test)] mod tests` at the bottom. Adapt each:

- Construct `Dispatcher::new(cursor_path)` with the test's chosen focus path
- Call `.dispatch(...)` instead of `dispatch_settings_key(...)`
- Drop `screen` and `mode` fixture fields

Coverage to preserve: capture out-ranks target; target out-ranks bubble; specificity ordering inside a phase; Anywhere is lowest tier; cursor missing → empty scope path → inert dispatch.

- [ ] **Step 2: Wire into horns-core**

```rust
// crates/horns-core/src/lib.rs
pub mod dispatch;
pub use dispatch::Dispatcher;
```

- [ ] **Step 3: Build and test horns-core**

Run: `cargo build -p horns-core && cargo test -p horns-core`
Expected: clean build, dispatch tests pass.

- [ ] **Step 4: Update `crates/ox-cli/src/settings/mod.rs`**

Remove `pub mod dispatch;`. The settings module no longer owns the dispatcher.

- [ ] **Step 5: Delete the old settings dispatch file**

```bash
rm /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-cli/src/settings/dispatch.rs
```

- [ ] **Step 6: Bridge `crates/ox-cli/src/dispatch.rs::send_key` to `Dispatcher`**

In `crates/ox-cli/src/dispatch.rs`, find the call to `settings::dispatch::dispatch_settings_key(...)`. Replace with:

```rust
use horns_core::Dispatcher;
use ox_path::oxpath;

let dispatcher = Dispatcher::new(oxpath!("ui", "settings", "focused"));
let writes = dispatcher.dispatch(
    &mut snap,
    &chord,
    bindings,
    commands,
    renderers,
);
```

(The variables `snap`, `chord`, `bindings`, `commands`, `renderers` should already be in scope — confirm by reading the surrounding function.)

This is a transitional bridge; Task 10 removes `ox-cli/src/dispatch.rs` entirely.

- [ ] **Step 7: Build and test the workspace**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass. Failing dispatch tests usually mean a test fixture still sets `mode:`; drop it.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
refactor: move dispatcher into horns-core as Dispatcher

dispatch_settings_key becomes Dispatcher::dispatch. The cursor path
the dispatcher reads is a constructor parameter, not a hardcoded oxpath
literal. ox-cli's send_key still calls into horns-core::Dispatcher for
the settings screen as a transitional bridge; Task 10 removes the
ox-cli dispatch wrapper entirely when event_loop migrates to broker
writes.
CM
)"
```

---

## Task 6: Move view_render and theme into horns-ratatui

**Files:**
- Create: `crates/horns-ratatui/src/render.rs` (from `crates/ox-cli/src/view_render.rs`)
- Create: `crates/horns-ratatui/src/theme.rs` (from `crates/ox-cli/src/theme.rs`)
- Modify: `crates/horns-ratatui/src/lib.rs`
- Delete: `crates/ox-cli/src/view_render.rs`
- Delete: `crates/ox-cli/src/theme.rs`
- Modify: every ox-cli file that imports `view_render::render_to_frame` or `theme::Theme`

- [ ] **Step 1: Copy the files**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
cp crates/ox-cli/src/view_render.rs crates/horns-ratatui/src/render.rs
cp crates/ox-cli/src/theme.rs       crates/horns-ratatui/src/theme.rs
```

- [ ] **Step 2: Fix imports in the moved files**

In `crates/horns-ratatui/src/render.rs`:
- Replace `use ox_view::*;` with `use horns_core::view::*;`
- Replace `use crate::theme::Theme;` — stays as `use crate::theme::Theme;` since both files now live in horns-ratatui

In `crates/horns-ratatui/src/theme.rs`:
- Replace `use ox_view::Color;` with `use horns_core::view::Color;`
- Confirm any other `crate::` references resolve under horns-ratatui (most theme code is self-contained)

- [ ] **Step 3: Wire `crates/horns-ratatui/src/lib.rs`**

```rust
//! horns-ratatui: ratatui backend mount for the horns framework.

pub mod render;
pub mod theme;

pub use render::render_to_frame;
pub use theme::{Theme, ledger_health_banner};
```

- [ ] **Step 4: Build horns-ratatui**

Run: `cargo build -p horns-ratatui`
Expected: clean build.

- [ ] **Step 5: Move snapshot fixtures so insta finds them**

The snapshot tests inside `render.rs` reference snapshot files in `crates/ox-cli/src/snapshots/`. Either:

a) Copy the relevant snapshots into `crates/horns-ratatui/src/snapshots/`:

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
mkdir -p crates/horns-ratatui/src/snapshots
grep -hn "insta::assert_snapshot!\|insta::assert_debug_snapshot!" crates/horns-ratatui/src/render.rs | \
  sed -E 's/.*assert[a-z_]*_snapshot!\(["]([^"]+)["].*/\1/' | sort -u
# Copy each matched snapshot file from crates/ox-cli/src/snapshots/ to crates/horns-ratatui/src/snapshots/
```

b) Or run `cargo insta accept` to regenerate the snapshots in their new location, after the tests run (with new outputs they may have already moved).

Choose (a) to preserve the existing assertions verbatim.

- [ ] **Step 6: Run horns-ratatui tests**

Run: `cargo test -p horns-ratatui`
Expected: snapshot tests pass. If a snapshot is missing, the test fails with a clear message; copy the corresponding fixture from `crates/ox-cli/src/snapshots/`.

- [ ] **Step 7: Update ox-cli imports**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
grep -rln "use crate::view_render\|use crate::theme\|view_render::render_to_frame\|theme::Theme" crates/ox-cli/src/
```

For each match:
- `crate::view_render::render_to_frame` → `horns_ratatui::render_to_frame`
- `crate::theme::Theme` → `horns_ratatui::Theme`
- `use crate::theme::*` → `use horns_ratatui::*`

Add `horns-ratatui = { path = "../horns-ratatui" }` to `crates/ox-cli/Cargo.toml` `[dependencies]` if not already present.

- [ ] **Step 8: Delete the originals from ox-cli**

```bash
rm /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-cli/src/view_render.rs
rm /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-cli/src/theme.rs
```

In `crates/ox-cli/src/lib.rs` (or `main.rs`), remove `pub mod view_render;` and `pub mod theme;`.

- [ ] **Step 9: Build and test the workspace**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
refactor: move view_render and theme into horns-ratatui

The ratatui translator and Theme leave ox-cli for horns-ratatui. ox-cli
imports horns_ratatui::render_to_frame and horns_ratatui::Theme directly
during the transition; Task 8 wraps both behind a horns-ratatui install
API mounted as a broker subscription.
CM
)"
```

---

## Task 7: Add the KeyHandler tier to BindingRegistry

**Files:**
- Modify: `crates/horns-core/src/binding.rs` — add `KeyHandler` trait, `HandlerEntry`, `HandlerMetadata`, `HandlerId`, `register_handler`, `lookup_handler`
- Modify: `crates/horns-core/src/dispatch.rs` — query the handler tier per-phase after discrete misses
- Modify: `crates/horns-core/src/lib.rs` — re-export new types

- [ ] **Step 1: Write a failing test in `crates/horns-core/src/binding.rs`**

Append to the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn lookup_handler_finds_handler_at_matching_scope_and_phase() {
    use crate::command::CommandCtx;
    use crate::write::Write;
    use std::sync::Arc;
    use structfs_core_store::{Path, Reader};

    struct AcceptAny;
    impl super::KeyHandler for AcceptAny {
        fn handle(
            &self,
            _: &mut dyn Reader,
            _: &crate::key::KeyChord,
            _: &CommandCtx<'_>,
        ) -> Option<Vec<Write>> {
            Some(vec![])
        }
    }

    let mut reg = BindingRegistry::new();
    let scope = BindingScope::Exact(Path::parse("a/b").unwrap());
    reg.register_handler(super::HandlerEntry {
        scope: scope.clone(),
        phase: Phase::Target,
        handler: Arc::new(AcceptAny),
    });

    let cursor = Path::parse("a/b").unwrap();
    let chord = crate::key::KeyChord {
        modifiers: Default::default(),
        code: crate::key::KeyCodeRepr::Char('x'),
    };

    assert!(reg.lookup_handler(&cursor, &chord, Phase::Target).is_some());
}

#[test]
fn lookup_handler_misses_when_phase_differs() {
    use crate::command::CommandCtx;
    use crate::write::Write;
    use std::sync::Arc;
    use structfs_core_store::{Path, Reader};

    struct NoOp;
    impl super::KeyHandler for NoOp {
        fn handle(&self, _: &mut dyn Reader, _: &crate::key::KeyChord, _: &CommandCtx<'_>)
            -> Option<Vec<Write>> { Some(vec![]) }
    }

    let mut reg = BindingRegistry::new();
    reg.register_handler(super::HandlerEntry {
        scope: BindingScope::Exact(Path::parse("a").unwrap()),
        phase: Phase::Capture,
        handler: Arc::new(NoOp),
    });

    let cursor = Path::parse("a").unwrap();
    let chord = crate::key::KeyChord {
        modifiers: Default::default(),
        code: crate::key::KeyCodeRepr::Esc,
    };
    assert!(reg.lookup_handler(&cursor, &chord, Phase::Bubble).is_none());
}
```

- [ ] **Step 2: Run the failing tests**

Run: `cargo test -p horns-core lookup_handler`
Expected: FAIL — `KeyHandler`, `HandlerEntry`, `register_handler`, `lookup_handler` undefined.

- [ ] **Step 3: Define the handler tier in `crates/horns-core/src/binding.rs`**

After the existing types, add:

```rust
use std::sync::Arc;
use structfs_core_store::Reader;

use crate::command::CommandCtx;
use crate::write::Write;

#[derive(Hash, Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HandlerId(pub String);

pub trait KeyHandler: Send + Sync {
    fn handle(
        &self,
        snapshot: &mut dyn Reader,
        key: &crate::key::KeyChord,
        ctx: &CommandCtx<'_>,
    ) -> Option<Vec<Write>>;
}

pub struct HandlerEntry {
    pub scope: BindingScope,
    pub phase: Phase,
    pub handler: Arc<dyn KeyHandler>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandlerMetadata {
    pub scope: BindingScope,
    pub phase: Phase,
    /// Free-form label naming what the handler claims, for introspection.
    /// Not interpreted by the framework. Examples: "printable_ascii",
    /// "arrow_navigation".
    pub class: String,
}
```

Extend `BindingRegistry` with a parallel handlers vec:

```rust
pub struct BindingRegistry {
    entries: Vec<BindingEntry>,
    handlers: Vec<HandlerEntry>,
}

impl BindingRegistry {
    pub fn new() -> Self {
        Self { entries: Vec::new(), handlers: Vec::new() }
    }

    pub fn register(&mut self, entry: BindingEntry) { self.entries.push(entry); }
    pub fn register_handler(&mut self, entry: HandlerEntry) { self.handlers.push(entry); }

    pub fn entries(&self) -> &[BindingEntry] { &self.entries }
    pub fn handlers(&self) -> &[HandlerEntry] { &self.handlers }

    // existing lookup() unchanged.

    /// First handler whose scope admits the cursor and whose phase matches.
    /// Registration order, first match wins.
    pub fn lookup_handler(
        &self,
        cursor: &structfs_core_store::Path,
        _key: &crate::key::KeyChord,
        phase: Phase,
    ) -> Option<&dyn KeyHandler> {
        for entry in &self.handlers {
            if entry.phase != phase { continue; }
            if !entry.scope.matches(cursor) { continue; }
            return Some(&*entry.handler);
        }
        None
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horns-core lookup_handler`
Expected: PASS for both tests.

- [ ] **Step 5: Teach the Dispatcher to query handlers**

In `crates/horns-core/src/dispatch.rs`, after each phase's discrete lookup, add the handler check at the same scope+phase. Capture becomes:

```rust
// Capture (outer → inner).
for scope in &scope_path {
    if let Some(p) = scope.keyed_path() {
        // Discrete first.
        if let Some(id) = bindings.lookup(p, key, Phase::Capture) {
            return run(commands, renderers, snapshot, key, id);
        }
        // Then handlers.
        if let Some(h) = bindings.lookup_handler(p, key, Phase::Capture) {
            let ctx = CommandCtx { last_keystroke: Some(key.clone()), renderers };
            if let Some(writes) = h.handle(snapshot, key, &ctx) {
                return writes;
            }
        }
    }
}
```

Apply the same pattern to Target (with `scope_path.last().and_then(BindingScope::keyed_path)`) and to Bubble (iterating `scope_path.iter().rev()`).

- [ ] **Step 6: Write a dispatcher test exercising the handler tier**

In `crates/horns-core/src/dispatch.rs` tests:

```rust
#[test]
fn dispatcher_routes_to_handler_when_discrete_misses() {
    use crate::binding::{BindingRegistry, BindingScope, HandlerEntry, KeyHandler, Phase};
    use crate::command::{CommandCtx, CommandRegistry};
    use crate::key::{KeyChord, KeyCodeRepr, KeyModifierSet};
    use crate::render::RendererRegistry;
    use crate::write::Write;
    use std::sync::Arc;
    use structfs_core_store::{Path, Reader, Record, Value};

    struct EatChar;
    impl KeyHandler for EatChar {
        fn handle(
            &self,
            _: &mut dyn Reader,
            k: &KeyChord,
            _: &CommandCtx<'_>,
        ) -> Option<Vec<Write>> {
            match k.code {
                KeyCodeRepr::Char(_) => Some(vec![]),
                _ => None,
            }
        }
    }

    // Use the same in-memory snapshot helper as the existing dispatch
    // tests use; if none exists, build one with ox-store-util's
    // LocalConfig or your own Reader impl.
    let mut snap = build_test_snapshot_with_cursor("a/b");

    let mut bindings = BindingRegistry::new();
    bindings.register_handler(HandlerEntry {
        scope: BindingScope::Exact(Path::parse("a/b").unwrap()),
        phase: Phase::Target,
        handler: Arc::new(EatChar),
    });
    let commands = CommandRegistry::new();
    let renderers = RendererRegistry::new();
    let dispatcher = Dispatcher::new(Path::parse("ui/test/cursor").unwrap());

    let chord = KeyChord {
        modifiers: KeyModifierSet::default(),
        code: KeyCodeRepr::Char('x'),
    };
    let writes = dispatcher.dispatch(&mut snap, &chord, &bindings, &commands, &renderers);
    assert_eq!(writes, vec![]);  // handler claimed with empty writes
}
```

(`build_test_snapshot_with_cursor` is a test helper that should already exist in the dispatch test module from the move in Task 5; if not, write one using `LocalConfig`.)

- [ ] **Step 7: Run the dispatcher test**

Run: `cargo test -p horns-core dispatcher_routes_to_handler`
Expected: PASS.

- [ ] **Step 8: Re-export from horns-core**

```rust
// crates/horns-core/src/lib.rs
pub use binding::{
    BindingEntry, BindingId, BindingRegistry, BindingScope, HandlerEntry,
    HandlerId, HandlerMetadata, KeyHandler, Phase,
};
```

- [ ] **Step 9: Build and test the workspace**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
feat: KeyHandler tier on BindingRegistry — opaque event consumers

A KeyHandler is an opaque closure registered against scope + phase.
The dispatcher's per-phase walk asks the discrete tier first (named,
introspectable); on miss, asks handlers in registration order; each
handler inspects the key and returns Some(writes) to claim or None
to pass.

This is the mechanism for a text field to claim 'any printable ASCII'
without enumerating 96 BindingEntries. Task 9 migrates the settings
_edit scope to use it.
CM
)"
```

---

## Task 8: Install API and runtime subscriptions

This is the largest new-code task. Read the spec section "The interface is StructFS" before starting; read `crates/ox-broker/src/subscription.rs` for the `Subscription`, `SubCtx`, `PathPattern` shapes you'll implement against; read `crates/ox-gate/src/subscriptions/account_test.rs` for a working subscription example.

**Files:**
- Create: `crates/horns-core/src/install.rs`
- Create: `crates/horns-core/src/subscription.rs`
- Create: `crates/horns-core/tests/install_smoke.rs`
- Create: `crates/horns-ratatui/src/install.rs`
- Modify: `crates/horns-core/src/lib.rs`
- Modify: `crates/horns-ratatui/src/lib.rs`
- Modify: `crates/horns-core/Cargo.toml` (add `parking_lot`, `anyhow`)

- [ ] **Step 1: Add dependencies to horns-core**

In `crates/horns-core/Cargo.toml` `[dependencies]`:

```toml
parking_lot = "0.12"
anyhow = "1"
```

- [ ] **Step 2: Define `InstallOptions` and `HornsHandle` in `crates/horns-core/src/install.rs`**

```rust
//! horns install API — register the framework as a broker mount.
//!
//! After install returns, the host's only horns interface is broker
//! writes. See `crates/horns/docs/architecture.md`.

use std::collections::HashMap;
use std::sync::Arc;

use ox_broker::{BrokerStore, SubscriptionId};
use parking_lot::RwLock;
use structfs_core_store::Path;

use crate::binding::{BindingEntry, BindingId, HandlerId, HandlerMetadata, KeyHandler};
use crate::command::{Command, CommandId};
use crate::render::Renderer;
use crate::subscription::SideTables;

pub struct InstallOptions {
    pub cursor_path: Path,
    pub input_path: Path,
    pub render_tick_path: Path,
    pub render_output_path: Path,
    pub bindings_prefix: Path,
    pub commands_prefix: Path,
    pub renderers_prefix: Path,
    pub handlers_prefix: Path,
    pub theme_path: Path,

    pub commands: HashMap<CommandId, Box<dyn Command>>,
    pub renderers: HashMap<Path, Box<dyn Renderer>>,
    pub handlers: HashMap<HandlerId, Arc<dyn KeyHandler>>,

    pub bindings: Vec<(BindingId, BindingEntry)>,
    pub handler_metadata: Vec<(HandlerId, HandlerMetadata)>,
    pub theme: serde_json::Value,
}

pub struct HornsHandle {
    pub(crate) side_tables: Arc<RwLock<SideTables>>,
    pub(crate) subscription_ids: Vec<SubscriptionId>,
}

impl HornsHandle {
    /// Subscription IDs registered by this install. Hosts that want to
    /// unmount call broker's SubscriptionRegistry::unregister on each.
    pub fn subscription_ids(&self) -> &[SubscriptionId] { &self.subscription_ids }
}

pub async fn install(
    broker: &mut BrokerStore,
    opts: InstallOptions,
) -> Result<HornsHandle, anyhow::Error> {
    let client = broker.client();

    // 1. Write all metadata to the broker.
    for (id, entry) in &opts.bindings {
        let path = opts.bindings_prefix.join(&id.0);
        client.write_typed(&path, entry).await?;
    }
    for (id, cmd) in &opts.commands {
        let meta = crate::command::CommandMetadata {
            display: cmd.display().clone(),
            scope: cmd.scope().clone(),
        };
        let path = opts.commands_prefix.join(&id.0);
        client.write_typed(&path, &meta).await?;
    }
    for (cursor, r) in &opts.renderers {
        let meta = crate::render::RendererMetadata { ascend_rule: r.ascend_to() };
        let path = opts.renderers_prefix.join(&cursor.to_string());
        client.write_typed(&path, &meta).await?;
    }
    for (id, meta) in &opts.handler_metadata {
        let path = opts.handlers_prefix.join(&id.0);
        client.write_typed(&path, meta).await?;
    }
    client.write_typed(&opts.theme_path, &opts.theme).await?;

    // 2. Move closures into shared side-tables.
    let side_tables = Arc::new(RwLock::new(SideTables {
        commands: opts.commands,
        renderers: opts.renderers,
        handlers: opts.handlers,
    }));

    // 3. Register the three subscriptions.
    let subscription_ids = crate::subscription::register_all(
        broker,
        &opts,
        side_tables.clone(),
    ).await?;

    Ok(HornsHandle { side_tables, subscription_ids })
}
```

- [ ] **Step 3: Implement subscriptions in `crates/horns-core/src/subscription.rs`**

**Read `crates/ox-broker/src/subscription.rs` first.** The `Subscription` trait has:

```rust
pub trait Subscription: Send + Sync {
    fn id(&self) -> &SubscriptionId;
    fn watches(&self) -> &[PathPattern];
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write>;
}
```

`SubCtx::snapshot` is a `&mut dyn Reader`. The handler returns `Vec<Write>` synchronously.

```rust
//! horns runtime subscriptions: KeyDispatch, Render, ThemeChange.

use std::collections::HashMap;
use std::sync::Arc;

use ox_broker::{
    BrokerStore, PathPattern, SubCtx, Subscription, SubscriptionId,
};
use parking_lot::RwLock;
use structfs_core_store::{Path, Reader};

use crate::binding::{BindingEntry, BindingId, BindingRegistry, HandlerEntry, HandlerId, HandlerMetadata, KeyHandler};
use crate::command::{Command, CommandId, CommandRegistry};
use crate::install::InstallOptions;
use crate::key::KeyChord;
use crate::render::{Renderer, RendererRegistry};
use crate::write::Write;
use crate::Dispatcher;

pub(crate) struct SideTables {
    pub commands: HashMap<CommandId, Box<dyn Command>>,
    pub renderers: HashMap<Path, Box<dyn Renderer>>,
    pub handlers: HashMap<HandlerId, Arc<dyn KeyHandler>>,
}

pub(crate) async fn register_all(
    broker: &mut BrokerStore,
    opts: &InstallOptions,
    side_tables: Arc<RwLock<SideTables>>,
) -> Result<Vec<SubscriptionId>, anyhow::Error> {
    let mut ids = Vec::new();

    let key_dispatch = KeyDispatchSubscription::new(opts, side_tables.clone());
    let render = RenderSubscription::new(opts, side_tables.clone());
    let theme_change = ThemeChangeSubscription::new(opts);

    ids.push(*key_dispatch.id());
    ids.push(*render.id());
    ids.push(*theme_change.id());

    // Read crates/ox-broker/src/subscription.rs for the actual registration API.
    // Likely: broker.subscriptions().register(Box::new(sub)).await
    broker.subscriptions().register(Box::new(key_dispatch)).await;
    broker.subscriptions().register(Box::new(render)).await;
    broker.subscriptions().register(Box::new(theme_change)).await;

    Ok(ids)
}

pub(crate) struct KeyDispatchSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    input_key_path: Path,
    render_tick_path: Path,
    dispatcher: Dispatcher,
    bindings_prefix: Path,
    handlers_prefix: Path,
    side_tables: Arc<RwLock<SideTables>>,
}

impl KeyDispatchSubscription {
    fn new(opts: &InstallOptions, side_tables: Arc<RwLock<SideTables>>) -> Self {
        let input_key_path = opts.input_path.join("key");
        Self {
            id: SubscriptionId::generate(),
            watches: vec![PathPattern::Exact(input_key_path.clone())],
            input_key_path,
            render_tick_path: opts.render_tick_path.clone(),
            dispatcher: Dispatcher::new(opts.cursor_path.clone()),
            bindings_prefix: opts.bindings_prefix.clone(),
            handlers_prefix: opts.handlers_prefix.clone(),
            side_tables,
        }
    }
}

impl Subscription for KeyDispatchSubscription {
    fn id(&self) -> &SubscriptionId { &self.id }
    fn watches(&self) -> &[PathPattern] { &self.watches }
    fn handle(&self, mut ctx: SubCtx<'_>) -> Vec<Write> {
        // Hydrate a transient BindingRegistry from the metadata subtree.
        let bindings = hydrate_bindings(ctx.snapshot, &self.bindings_prefix, &self.handlers_prefix, &self.side_tables);

        // Hydrate transient Command/Renderer registries from the side-tables.
        let tables = self.side_tables.read();
        let mut commands = CommandRegistry::new();
        for (_, cmd) in tables.commands.iter() {
            commands.register(clone_command_arc(cmd));
        }
        let mut renderers = RendererRegistry::new();
        for (cursor, r) in tables.renderers.iter() {
            renderers.register(cursor.clone(), clone_renderer_arc(r));
        }

        // Read the key chord and dispatch.
        let Some(chord) = read_key_chord(ctx.snapshot, &self.input_key_path) else {
            return vec![];
        };
        let mut writes = self.dispatcher.dispatch(ctx.snapshot, &chord, &bindings, &commands, &renderers);

        // Bump render-tick to trigger a redraw.
        writes.push(make_tick_bump(&self.render_tick_path, ctx.snapshot));
        writes
    }
}

// Sketches for the two helpers — implementations are short:
fn hydrate_bindings(
    _snap: &mut dyn Reader,
    _bindings_prefix: &Path,
    _handlers_prefix: &Path,
    _side_tables: &Arc<RwLock<SideTables>>,
) -> BindingRegistry {
    // 1. Walk <bindings_prefix>/* via snapshot reads, deserialize each
    //    Record as BindingEntry, push into a new BindingRegistry.
    // 2. Walk <handlers_prefix>/* — for each HandlerId, look up the
    //    Arc<dyn KeyHandler> in side_tables.handlers and push as a
    //    HandlerEntry.
    todo!("implement subtree-read using snapshot; see ox-broker docs for the read shape")
}

fn read_key_chord(_snap: &mut dyn Reader, _path: &Path) -> Option<KeyChord> {
    // snap.read(path)?.as_value()?.deserialize_as::<KeyChord>()?
    todo!()
}

fn make_tick_bump(_path: &Path, _snap: &mut dyn Reader) -> Write {
    // read current u64; write next.
    todo!()
}

// SubscriptionId and the other registries can't be cloned in the
// borrow-strict sense, so we use Arc<dyn Trait> internally when we
// must share across handler invocations. The full type-juggling is
// fiddly — write integration tests first (step 4) and iterate.
fn clone_command_arc(_: &Box<dyn Command>) -> Box<dyn Command> { todo!() }
fn clone_renderer_arc(_: &Box<dyn Renderer>) -> Box<dyn Renderer> { todo!() }

// RenderSubscription and ThemeChangeSubscription follow the same shape.
pub(crate) struct RenderSubscription { /* fields */ }
impl RenderSubscription { fn new(_o: &InstallOptions, _s: Arc<RwLock<SideTables>>) -> Self { todo!() } }
impl Subscription for RenderSubscription {
    fn id(&self) -> &SubscriptionId { todo!() }
    fn watches(&self) -> &[PathPattern] { todo!() }
    fn handle(&self, _ctx: SubCtx<'_>) -> Vec<Write> {
        // 1. Read cursor from cursor_path.
        // 2. Look up renderer for cursor in side_tables.renderers.
        // 3. Read theme from theme_path.
        // 4. Build RenderCtx; run renderer.
        // 5. Return [Write { path: render_output_path, record: View as JSON }].
        todo!()
    }
}

pub(crate) struct ThemeChangeSubscription { /* fields */ }
impl ThemeChangeSubscription { fn new(_o: &InstallOptions) -> Self { todo!() } }
impl Subscription for ThemeChangeSubscription {
    fn id(&self) -> &SubscriptionId { todo!() }
    fn watches(&self) -> &[PathPattern] { todo!() }
    fn handle(&self, _ctx: SubCtx<'_>) -> Vec<Write> {
        // Bump render-tick.
        todo!()
    }
}
```

**Hot spots to resolve while implementing:**

1. **Cloning the registries on every dispatch.** Rebuilding `CommandRegistry`/`RendererRegistry` from `Box<dyn>` requires either (a) sharing via `Arc<dyn Command>` in the side-tables instead of `Box<dyn>`, or (b) holding read-locks for the duration of dispatch. Prefer (a) — change the side-table types to `HashMap<CommandId, Arc<dyn Command>>` etc.

2. **`SubscriptionId::generate()`** — confirm the actual constructor in `crates/ox-broker/src/subscription.rs`. It might be `SubscriptionId::new()` or take a string.

3. **`broker.subscriptions()`** — the exact accessor name on `BrokerStore`. Read the broker's public API.

The implementation guidance above is a sketch; **the integration test in step 4 is what drives the actual implementation to correctness**. Don't try to "finish" the subscription file before writing the test.

- [ ] **Step 4: Write the failing install smoke test**

Create `crates/horns-core/tests/install_smoke.rs`:

```rust
use std::collections::HashMap;
use std::time::Duration;

use horns_core::{
    install::{install, InstallOptions},
    BindingEntry, BindingId, BindingScope, CommandId, KeyChord, KeyCodeRepr,
    KeyModifierSet, Phase,
};
use ox_broker::BrokerStore;
use ox_path::oxpath;
use structfs_core_store::Reader;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_writes_binding_metadata_to_broker() {
    let mut broker = BrokerStore::new(Duration::from_secs(5));

    let opts = InstallOptions {
        cursor_path: oxpath!("ui", "test", "focused"),
        input_path: oxpath!("ui", "test", "input"),
        render_tick_path: oxpath!("ui", "test", "render", "tick"),
        render_output_path: oxpath!("ui", "test", "render", "output"),
        bindings_prefix: oxpath!("horns", "test", "bindings"),
        commands_prefix: oxpath!("horns", "test", "commands"),
        renderers_prefix: oxpath!("horns", "test", "renderers"),
        handlers_prefix: oxpath!("horns", "test", "handlers"),
        theme_path: oxpath!("ui", "test", "theme"),
        commands: HashMap::new(),
        renderers: HashMap::new(),
        handlers: HashMap::new(),
        bindings: vec![(
            BindingId("test.noop".into()),
            BindingEntry {
                scope: BindingScope::Anywhere,
                key: KeyChord {
                    modifiers: KeyModifierSet::default(),
                    code: KeyCodeRepr::Esc,
                },
                phase: Phase::Bubble,
                command_id: CommandId("test.noop".into()),
            },
        )],
        handler_metadata: vec![],
        theme: serde_json::json!({}),
    };

    let _handle = install(&mut broker, opts).await.expect("install");

    let client = broker.client();
    let read = client
        .read(&oxpath!("horns", "test", "bindings", "test.noop"))
        .await
        .expect("read");
    assert!(read.is_some(), "binding metadata should be present at the path after install");
}
```

- [ ] **Step 5: Run the test (will fail until subscription stubs are filled in)**

Run: `cargo test -p horns-core --test install_smoke`
Expected: FAIL — `todo!()` in `register_all` or earlier.

- [ ] **Step 6: Fill in subscriptions until the test passes**

Iterate: hit each `todo!()`, implement, re-run. The fix order that minimizes churn:

a) Change side-tables to `Arc<dyn Trait>` so cloning works.
b) Make `register_all` actually call the broker's subscription-registration API.
c) Make `make_tick_bump` and `read_key_chord` and `hydrate_bindings` actually read/write through the snapshot reader.

The smoke test only exercises metadata writes — it doesn't trigger any subscription. So `register_all` returning `Ok(vec![ids...])` is enough; the subscription handlers can stay `todo!()` until step 7.

Run repeatedly: `cargo test -p horns-core --test install_smoke`
Expected: PASS when metadata writes work.

- [ ] **Step 7: Write the end-to-end key-write → render test**

Append to `crates/horns-core/tests/install_smoke.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_write_triggers_command_dispatch_and_render() {
    // Goal: write a KeyChord to the input path; observe a command-produced
    // write at a test-witness path; observe a View at the render output.

    let mut broker = BrokerStore::new(Duration::from_secs(5));

    // Build one Command that writes to a witness path.
    struct WitnessCmd;
    impl horns_core::Command for WitnessCmd {
        fn id(&self) -> &horns_core::CommandId {
            static ID: once_cell::sync::Lazy<horns_core::CommandId> =
                once_cell::sync::Lazy::new(|| horns_core::CommandId("test.witness".into()));
            &ID
        }
        fn display(&self) -> &horns_core::CommandDisplay {
            static D: once_cell::sync::Lazy<horns_core::CommandDisplay> =
                once_cell::sync::Lazy::new(|| horns_core::CommandDisplay {
                    name: "witness".into(),
                    description: "test witness".into(),
                });
            &D
        }
        fn scope(&self) -> &horns_core::CommandScope {
            static S: once_cell::sync::Lazy<horns_core::CommandScope> =
                once_cell::sync::Lazy::new(|| horns_core::CommandScope { cursor_path: None });
            &S
        }
        fn run(
            &self,
            _snap: &mut dyn Reader,
            _ctx: &horns_core::CommandCtx<'_>,
        ) -> Vec<horns_core::Write> {
            vec![horns_core::Write {
                path: oxpath!("ui", "test", "witness"),
                record: structfs_core_store::Record::parsed(
                    structfs_core_store::Value::Bool(true)
                ),
            }]
        }
    }

    // Build one Renderer for the test cursor.
    struct EmptyRenderer;
    impl horns_core::Renderer for EmptyRenderer {
        fn render(&self, _ctx: &mut horns_core::RenderCtx<'_>) -> horns_core::View {
            horns_core::View::Empty
        }
        fn ascend_to(&self) -> horns_core::AscendRule {
            horns_core::AscendRule::ExitScreen
        }
    }

    let mut commands = HashMap::new();
    commands.insert(
        horns_core::CommandId("test.witness".into()),
        Box::new(WitnessCmd) as Box<dyn horns_core::Command>,
    );
    let mut renderers = HashMap::new();
    renderers.insert(
        oxpath!("test", "page"),
        Box::new(EmptyRenderer) as Box<dyn horns_core::Renderer>,
    );

    let opts = InstallOptions {
        cursor_path: oxpath!("ui", "test", "focused"),
        input_path: oxpath!("ui", "test", "input"),
        render_tick_path: oxpath!("ui", "test", "render", "tick"),
        render_output_path: oxpath!("ui", "test", "render", "output"),
        bindings_prefix: oxpath!("horns", "test", "bindings"),
        commands_prefix: oxpath!("horns", "test", "commands"),
        renderers_prefix: oxpath!("horns", "test", "renderers"),
        handlers_prefix: oxpath!("horns", "test", "handlers"),
        theme_path: oxpath!("ui", "test", "theme"),
        commands,
        renderers,
        handlers: HashMap::new(),
        bindings: vec![(
            BindingId("test.witness".into()),
            BindingEntry {
                scope: BindingScope::Anywhere,
                key: KeyChord {
                    modifiers: KeyModifierSet::default(),
                    code: KeyCodeRepr::Char('w'),
                },
                phase: Phase::Bubble,
                command_id: CommandId("test.witness".into()),
            },
        )],
        handler_metadata: vec![],
        theme: serde_json::json!({}),
    };

    let _handle = install(&mut broker, opts).await.expect("install");

    let client = broker.client();
    // Seed the cursor.
    client.write_typed(
        &oxpath!("ui", "test", "focused"),
        &"test/page".to_string(),
    ).await.expect("seed cursor");

    // Write the key chord.
    client.write_typed(
        &oxpath!("ui", "test", "input", "key"),
        &KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char('w'),
        },
    ).await.expect("write chord");

    // Let the cascade settle.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Confirm the witness write happened.
    let witness = client.read(&oxpath!("ui", "test", "witness")).await
        .expect("read witness")
        .expect("witness present");
    assert!(matches!(
        witness.as_value(),
        Some(structfs_core_store::Value::Bool(true))
    ));

    // Confirm the render output was written.
    let out = client.read(&oxpath!("ui", "test", "render", "output")).await
        .expect("read output")
        .expect("output present");
    // Should be a serialized View::Empty.
    let _value = out.as_value().expect("value");
}
```

Add `once_cell = "1"` to `horns-core/Cargo.toml` `[dev-dependencies]` for the test.

- [ ] **Step 8: Drive the end-to-end test to green**

Run: `cargo test -p horns-core --test install_smoke key_write_triggers`

This will fail in multiple places — fix each in `subscription.rs`:

- Fill in `KeyDispatchSubscription::handle` so it actually dispatches.
- Fill in `RenderSubscription::handle` so it writes a View to the output path.
- Fill in `ThemeChangeSubscription::handle` so it bumps render-tick.

**Do not move on to step 9 until this test passes.** It's the canonical end-to-end correctness check for the install API.

- [ ] **Step 9: Re-export from horns-core**

```rust
// crates/horns-core/src/lib.rs
pub mod install;
pub(crate) mod subscription;

pub use install::{install, HornsHandle, InstallOptions};
```

- [ ] **Step 10: Implement `horns-ratatui::install`**

Create `crates/horns-ratatui/src/install.rs`:

```rust
//! horns-ratatui install: ratatui view-render mount.

use std::sync::Arc;

use horns_core::View;
use ox_broker::{
    BrokerStore, PathPattern, SubCtx, Subscription, SubscriptionId,
};
use parking_lot::Mutex;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Stdout;
use structfs_core_store::Path;

pub struct RatatuiOptions {
    pub view_input_path: Path,
    pub theme_path: Path,
    pub terminal: Arc<Mutex<Terminal<CrosstermBackend<Stdout>>>>,
}

pub struct RatatuiHandle {
    pub subscription_id: SubscriptionId,
}

pub async fn install(
    broker: &mut BrokerStore,
    opts: RatatuiOptions,
) -> Result<RatatuiHandle, anyhow::Error> {
    let sub = ViewRenderSubscription {
        id: SubscriptionId::generate(),
        watches: vec![PathPattern::Exact(opts.view_input_path.clone())],
        view_input_path: opts.view_input_path,
        theme_path: opts.theme_path,
        terminal: opts.terminal,
    };
    let id = *sub.id();
    broker.subscriptions().register(Box::new(sub)).await;
    Ok(RatatuiHandle { subscription_id: id })
}

struct ViewRenderSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    view_input_path: Path,
    theme_path: Path,
    terminal: Arc<Mutex<Terminal<CrosstermBackend<Stdout>>>>,
}

impl Subscription for ViewRenderSubscription {
    fn id(&self) -> &SubscriptionId { &self.id }
    fn watches(&self) -> &[PathPattern] { &self.watches }
    fn handle(&self, mut ctx: SubCtx<'_>) -> Vec<horns_core::Write> {
        // 1. Read the View JSON from ctx.snapshot at view_input_path.
        let view: View = match ctx.snapshot.read(&self.view_input_path) {
            Ok(Some(rec)) => match rec.as_value() {
                Some(v) => match serde_json::from_value(v.clone().into()) {
                    Ok(view) => view,
                    Err(e) => {
                        tracing::error!(error = %e, "horns-ratatui: bad View JSON");
                        return vec![];
                    }
                },
                None => return vec![],
            },
            _ => return vec![],
        };

        // 2. Read theme (currently a serde_json::Value; ratatui translator
        //    expects a concrete Theme. Deserialize or use a default).
        let theme = crate::theme::Theme::default_theme();

        // 3. Draw to the terminal.
        let mut term = self.terminal.lock();
        if let Err(e) = term.draw(|frame| {
            let area = frame.size();
            crate::render::render_to_frame(&view, frame, area, &theme);
        }) {
            tracing::error!(error = %e, "horns-ratatui: terminal draw failed");
        }

        vec![]  // backend produces no writes
    }
}
```

Add `anyhow = "1"`, `parking_lot = "0.12"`, `tracing = { workspace = true }`, `serde_json = "1"` to `crates/horns-ratatui/Cargo.toml` `[dependencies]` if not already present.

In `crates/horns-ratatui/src/lib.rs`:

```rust
pub mod install;
pub mod render;
pub mod theme;

pub use install::{install, RatatuiHandle, RatatuiOptions};
pub use render::render_to_frame;
pub use theme::Theme;
```

- [ ] **Step 11: Build the workspace and run all tests**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
feat: install API and runtime subscriptions for horns

horns::install(broker, options) registers three subscriptions
(KeyDispatch, Render, ThemeChange) and side-tables for in-process
closures (Commands, Renderers, KeyHandlers). After install returns,
all subsequent interaction is broker writes.

horns_ratatui::install registers a ViewRenderSubscription that
watches the configured view-input path and draws each new View to
a ratatui Terminal.

End-to-end test: write a KeyChord; observe the matched Command's
witness write AND the rendered View on broker paths.
CM
)"
```

---

## Task 9: Migrate the settings _edit scope from 96 BindingEntries to one TextInputHandler

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/edit.rs` — add `TextInputHandler` struct
- Modify: `crates/ox-cli/src/settings/bindings.rs` — replace 96 printable-ASCII discrete bindings with one handler registration; keep 4 lifecycle bindings (Backspace, Enter, Esc, Tab) as discrete

- [ ] **Step 1: Confirm the existing _edit behavior is locked by tests**

```bash
cargo test -p ox-cli edit
cargo test -p ox-cli settings_e2e
```

Both should pass before any changes. If they fail, fix what's broken before refactoring.

- [ ] **Step 2: Write a failing test for the handler-based path**

In `crates/ox-cli/src/settings/commands/edit.rs`, add a new test in the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn text_input_handler_inserts_printable_ascii() {
    use horns_core::{CommandCtx, RendererRegistry};
    use horns_core::{KeyChord, KeyCodeRepr, KeyModifierSet};
    use ox_path::oxpath;
    use ox_store_util::local_config::LocalConfig;
    use structfs_core_store::{Reader, Value};

    let buffer_path = oxpath!("ui", "settings", "edit", "buffer");
    let cursor_index_path = oxpath!("ui", "settings", "edit", "cursor_index");

    let mut snap = LocalConfig::new();
    snap.set(&buffer_path.to_string(), Value::String("ab".into()));
    snap.set(&cursor_index_path.to_string(), Value::Int(1));

    let handler = TextInputHandler::new(buffer_path.clone(), cursor_index_path.clone());
    let chord = KeyChord {
        modifiers: KeyModifierSet::default(),
        code: KeyCodeRepr::Char('z'),
    };
    let renderers = RendererRegistry::new();
    let ctx = CommandCtx {
        last_keystroke: Some(chord.clone()),
        renderers: &renderers,
    };

    let writes = horns_core::KeyHandler::handle(&handler, &mut snap as &mut dyn Reader, &chord, &ctx)
        .expect("printable ascii is claimed");
    // Expect: one write to buffer_path setting "azb", one to cursor_index_path setting 2.
    assert_eq!(writes.len(), 2);
    let mut got_buffer = false;
    let mut got_index = false;
    for w in &writes {
        if w.path == buffer_path {
            assert_eq!(w.record.as_value(), Some(&Value::String("azb".into())));
            got_buffer = true;
        }
        if w.path == cursor_index_path {
            assert_eq!(w.record.as_value(), Some(&Value::Int(2)));
            got_index = true;
        }
    }
    assert!(got_buffer && got_index);
}

#[test]
fn text_input_handler_passes_on_non_printable() {
    use horns_core::{CommandCtx, RendererRegistry};
    use horns_core::{KeyChord, KeyCodeRepr, KeyModifierSet};
    use ox_path::oxpath;
    use ox_store_util::local_config::LocalConfig;
    use structfs_core_store::Reader;

    let handler = TextInputHandler::new(
        oxpath!("ui", "settings", "edit", "buffer"),
        oxpath!("ui", "settings", "edit", "cursor_index"),
    );

    let mut snap = LocalConfig::new();
    let chord = KeyChord {
        modifiers: KeyModifierSet::default(),
        code: KeyCodeRepr::Enter,
    };
    let renderers = RendererRegistry::new();
    let ctx = CommandCtx { last_keystroke: Some(chord.clone()), renderers: &renderers };

    let result = horns_core::KeyHandler::handle(&handler, &mut snap as &mut dyn Reader, &chord, &ctx);
    assert!(result.is_none(), "non-printable should NOT be claimed");
}

#[test]
fn text_input_handler_passes_on_modified_key() {
    use horns_core::{CommandCtx, RendererRegistry};
    use horns_core::{KeyChord, KeyCodeRepr, KeyModifierSet};
    use ox_path::oxpath;
    use ox_store_util::local_config::LocalConfig;
    use structfs_core_store::Reader;

    let handler = TextInputHandler::new(
        oxpath!("ui", "settings", "edit", "buffer"),
        oxpath!("ui", "settings", "edit", "cursor_index"),
    );

    let mut snap = LocalConfig::new();
    let mut mods = KeyModifierSet::default();
    mods.ctrl = true;
    let chord = KeyChord {
        modifiers: mods,
        code: KeyCodeRepr::Char('a'),
    };
    let renderers = RendererRegistry::new();
    let ctx = CommandCtx { last_keystroke: Some(chord.clone()), renderers: &renderers };

    let result = horns_core::KeyHandler::handle(&handler, &mut snap as &mut dyn Reader, &chord, &ctx);
    assert!(result.is_none(), "Ctrl+letter should NOT be claimed by text input");
}
```

- [ ] **Step 3: Run the failing tests**

Run: `cargo test -p ox-cli text_input_handler`
Expected: FAIL — `TextInputHandler` undefined.

- [ ] **Step 4: Implement TextInputHandler in `crates/ox-cli/src/settings/commands/edit.rs`**

Find the existing `EditInsertChar` command's `run` body. Replicate its logic in a handler. The handler reads the chord from its argument rather than `CommandCtx::last_keystroke`.

```rust
use std::sync::Arc;

use horns_core::{CommandCtx, KeyChord, KeyCodeRepr, KeyHandler, Write};
use structfs_core_store::{Path, Reader, Record, Value};

pub struct TextInputHandler {
    buffer_path: Path,
    cursor_index_path: Path,
}

impl TextInputHandler {
    pub fn new(buffer_path: Path, cursor_index_path: Path) -> Self {
        Self { buffer_path, cursor_index_path }
    }
}

impl KeyHandler for TextInputHandler {
    fn handle(
        &self,
        snapshot: &mut dyn Reader,
        key: &KeyChord,
        _ctx: &CommandCtx<'_>,
    ) -> Option<Vec<Write>> {
        // Only claim un-modified printable chars. Shift is allowed
        // because uppercase letters arrive as shift+lowercase chars.
        if key.modifiers.ctrl || key.modifiers.alt || key.modifiers.super_ {
            return None;
        }
        let ch = match &key.code {
            KeyCodeRepr::Char(c) if !c.is_control() => *c,
            _ => return None,
        };

        // Read current buffer + cursor_index from the snapshot.
        let buffer = snapshot.read(&self.buffer_path).ok().flatten()
            .and_then(|r| r.as_value().and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_default();
        let cursor_index = snapshot.read(&self.cursor_index_path).ok().flatten()
            .and_then(|r| r.as_value().and_then(|v| v.as_int().map(|i| i as usize)))
            .unwrap_or(0);

        let clamped = cursor_index.min(buffer.chars().count());
        let mut new_buffer = String::with_capacity(buffer.len() + ch.len_utf8());
        for (i, existing) in buffer.chars().enumerate() {
            if i == clamped {
                new_buffer.push(ch);
            }
            new_buffer.push(existing);
        }
        if clamped == buffer.chars().count() {
            new_buffer.push(ch);
        }

        Some(vec![
            Write {
                path: self.buffer_path.clone(),
                record: Record::parsed(Value::String(new_buffer)),
            },
            Write {
                path: self.cursor_index_path.clone(),
                record: Record::parsed(Value::Int((clamped + 1) as i64)),
            },
        ])
    }
}
```

If the existing `EditInsertChar::run` differs from this (e.g., it reads from different paths, or it has additional bookkeeping like an error reset write), port those details too. Read its body line-by-line and replicate.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p ox-cli text_input_handler`
Expected: PASS.

- [ ] **Step 6: Replace the 96 _edit BindingEntries with one handler**

Find the relevant section in `crates/ox-cli/src/settings/bindings.rs`:

```bash
grep -n "_edit\|edit.insert_char" /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-cli/src/settings/bindings.rs | head -40
```

Find the loop or batch that registers one BindingEntry per printable ASCII char with `command_id = CommandId("edit.insert_char")`. Replace with a handler registration:

```rust
use horns_core::{HandlerEntry, HandlerId, HandlerMetadata};
use std::sync::Arc;
use ox_path::oxpath;

// Register the handler (code side):
let text_handler = Arc::new(
    crate::settings::commands::edit::TextInputHandler::new(
        oxpath!("ui", "settings", "edit", "buffer"),
        oxpath!("ui", "settings", "edit", "cursor_index"),
    ),
);
handlers.insert(HandlerId("settings.edit.printable".into()), text_handler);
handler_metadata.push((
    HandlerId("settings.edit.printable".into()),
    HandlerMetadata {
        scope: horns_core::BindingScope::Exact(oxpath!("settings", "_edit")),
        phase: horns_core::Phase::Target,
        class: "printable_ascii".into(),
    },
));
```

This step assumes `bindings::register_all` already takes `&mut HashMap<HandlerId, Arc<dyn KeyHandler>>` and `&mut Vec<(HandlerId, HandlerMetadata)>`. If it doesn't yet (signatures still take `&mut BindingRegistry`), that's Task 10 — for this task, keep the BindingRegistry mutation pattern and just call `.register_handler(...)` on it directly.

The 4 lifecycle bindings (Backspace, Enter, Esc, Tab under `Exact(settings/_edit)`) **stay as discrete BindingEntries**. They're enumerable in the help screen and overridable on disk.

- [ ] **Step 7: Build and run the settings test suite**

Run: `cargo test -p ox-cli settings`
Expected: every settings test passes. Crucially, the existing "typing X while editing inserts X" tests should pass through the handler path now.

If a test was asserting on the BindingRegistry's entries by counting them or checking specific BindingEntry presence, update it to query handlers or to test the user-visible behavior via dispatch.

- [ ] **Step 8: Add a regression assertion that the _edit table shrank**

In `crates/ox-cli/src/settings/bindings.rs` tests (or as a test under settings/):

```rust
#[test]
fn settings_edit_scope_has_no_more_than_six_discrete_bindings() {
    use horns_core::BindingScope;
    // The intent: 4 lifecycle (Backspace, Enter, Esc, Tab) plus a small
    // headroom. If this hits 96, the handler migration regressed.
    let mut bindings = horns_core::BindingRegistry::new();
    let mut handlers = std::collections::HashMap::new();
    let mut handler_metadata = Vec::new();
    super::register_all(&mut /* bindings vec */, &mut handlers, &mut handler_metadata);
    // Convert the vec back into a registry to inspect entries:
    // (Use whichever shape register_all settled on.)

    let edit_count = bindings.entries().iter()
        .filter(|e| matches!(
            &e.scope,
            BindingScope::Exact(p) if p.components.last().map(|c| c.as_str()) == Some("_edit")
        ))
        .count();
    assert!(
        edit_count <= 6,
        "expected ≤6 discrete bindings under _edit, got {edit_count}"
    );
}
```

Run: `cargo test -p ox-cli settings_edit_scope_has_no_more_than_six_discrete_bindings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
refactor: migrate settings _edit scope to TextInputHandler

The settings edit scope previously registered 96 discrete BindingEntries —
one per printable ASCII char — to route to edit.insert_char. Replace with
a single TextInputHandler at Phase::Target that inspects the chord and
returns the same writes the EditInsertChar command produced. Lifecycle
keys (Backspace, Enter, Esc, Tab) remain discrete so the help screen
can still enumerate them.

This is the canonical demonstration of horns' opaque-handler
encapsulation; future reusable widgets follow the same pattern.
CM
)"
```

---

## Task 10: Rewire ox-cli's event loop and main to use horns::install

**Files:**
- Modify: `crates/ox-cli/src/settings/mod.rs` — add `pub async fn install(broker, terminal) -> SettingsHandle`
- Modify: `crates/ox-cli/src/settings/bindings.rs` — change `register_all` signature to build `Vec<(BindingId, BindingEntry)>` instead of mutating a BindingRegistry
- Modify: `crates/ox-cli/src/settings/commands/mod.rs` — change `register_all` signature to build `HashMap<CommandId, Box<dyn Command>>`
- Modify: `crates/ox-cli/src/settings/renderers/mod.rs` — same shape, `HashMap<Path, Box<dyn Renderer>>`
- Modify: `crates/ox-cli/src/event_loop.rs` — write KeyChord to broker path instead of calling dispatch::send_key
- Modify: `crates/ox-cli/src/main.rs` (or `app.rs`) — call `settings::install` at startup
- Delete: `crates/ox-cli/src/dispatch.rs`

- [ ] **Step 1: Change `register_all` signatures**

In `crates/ox-cli/src/settings/commands/mod.rs`:

```rust
use std::collections::HashMap;
use horns_core::{Command, CommandId};

pub fn register_all(out: &mut HashMap<CommandId, Box<dyn Command>>) {
    out.insert(
        CommandId("accounts.add".into()),
        Box::new(highlight::HighlightAccountsNext::new()),
    );
    // ... one insert per command (port from the existing BindingRegistry-based register_all) ...
}
```

In `crates/ox-cli/src/settings/renderers/mod.rs`:

```rust
use std::collections::HashMap;
use horns_core::Renderer;
use structfs_core_store::Path;
use ox_path::oxpath;

pub fn register_all(out: &mut HashMap<Path, Box<dyn Renderer>>) {
    out.insert(oxpath!("settings", "index"), Box::new(index::IndexRenderer::new()));
    // ... one insert per renderer ...
}
```

In `crates/ox-cli/src/settings/bindings.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use horns_core::{
    BindingEntry, BindingId, BindingScope, HandlerId, HandlerMetadata,
    KeyChord, KeyCodeRepr, KeyHandler, Phase, CommandId,
};
use ox_path::oxpath;

pub fn register_all(
    bindings: &mut Vec<(BindingId, BindingEntry)>,
    handlers: &mut HashMap<HandlerId, Arc<dyn KeyHandler>>,
    handler_metadata: &mut Vec<(HandlerId, HandlerMetadata)>,
) {
    bindings.push((
        BindingId("settings.accounts.add".into()),
        BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings")),
            key: KeyChord { /* ... */ },
            phase: Phase::Bubble,
            command_id: CommandId("accounts.add".into()),
        },
    ));
    // ... one push per binding (port from the existing entries) ...

    // Handlers:
    let text_handler = Arc::new(
        crate::settings::commands::edit::TextInputHandler::new(
            oxpath!("ui", "settings", "edit", "buffer"),
            oxpath!("ui", "settings", "edit", "cursor_index"),
        ),
    );
    let handler_id = HandlerId("settings.edit.printable".into());
    handlers.insert(handler_id.clone(), text_handler);
    handler_metadata.push((handler_id, HandlerMetadata {
        scope: BindingScope::Exact(oxpath!("settings", "_edit")),
        phase: Phase::Target,
        class: "printable_ascii".into(),
    }));
}
```

This is the bulk of the task — porting ~30+ binding entries and ~20+ commands. Mechanical but tedious. Run `cargo build -p ox-cli` after each ~5 entries to catch mistakes early.

- [ ] **Step 2: Add `settings::install` to `crates/ox-cli/src/settings/mod.rs`**

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::io::Stdout;

use anyhow::Result;
use horns::install::{install as horns_install, HornsHandle, InstallOptions};
use horns::ratatui::install::{install as horns_ratatui_install, RatatuiHandle, RatatuiOptions};
use ox_broker::BrokerStore;
use ox_path::oxpath;
use parking_lot::Mutex;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub mod bindings;
pub mod bootstrap;
pub mod commands;
pub mod help;
pub mod renderers;
pub mod snapshot;
pub mod visible_rows;

pub struct SettingsHandle {
    pub horns: HornsHandle,
    pub ratatui: RatatuiHandle,
}

pub async fn install(
    broker: &mut BrokerStore,
    terminal: Arc<Mutex<Terminal<CrosstermBackend<Stdout>>>>,
) -> Result<SettingsHandle> {
    let mut commands = HashMap::new();
    let mut renderers = HashMap::new();
    let mut handlers = HashMap::new();
    let mut handler_metadata = Vec::new();
    let mut bindings_list = Vec::new();

    commands::register_all(&mut commands);
    renderers::register_all(&mut renderers);
    bindings::register_all(&mut bindings_list, &mut handlers, &mut handler_metadata);

    let horns = horns_install(broker, InstallOptions {
        cursor_path:        oxpath!("ui", "settings", "focused"),
        input_path:         oxpath!("ui", "_horns", "settings", "input"),
        render_tick_path:   oxpath!("ui", "_horns", "settings", "render", "tick"),
        render_output_path: oxpath!("ui", "_horns", "settings", "render", "output"),
        bindings_prefix:    oxpath!("horns", "settings", "bindings"),
        commands_prefix:    oxpath!("horns", "settings", "commands"),
        renderers_prefix:   oxpath!("horns", "settings", "renderers"),
        handlers_prefix:    oxpath!("horns", "settings", "handlers"),
        theme_path:         oxpath!("ui", "theme"),
        commands,
        renderers,
        handlers,
        bindings: bindings_list,
        handler_metadata,
        theme: serde_json::json!({}),
    }).await?;

    let ratatui = horns_ratatui_install(broker, RatatuiOptions {
        view_input_path: oxpath!("ui", "_horns", "settings", "render", "output"),
        theme_path:      oxpath!("ui", "theme"),
        terminal,
    }).await?;

    Ok(SettingsHandle { horns, ratatui })
}
```

Replace `ox-cli/Cargo.toml`'s `horns-core` direct dep (added in Task 2) with the umbrella:

```toml
horns = { path = "../horns" }
```

(Keep `horns-core` as a transitive dep through `horns`; remove the explicit line.)

- [ ] **Step 3: Rewire `crates/ox-cli/src/event_loop.rs`**

Find the section that calls `dispatch::send_key` (or routes the key to the settings dispatcher). Replace with a path write:

```rust
use ox_path::oxpath;

// ... parse the crossterm event into a KeyChord (existing helper) ...
let chord: horns::KeyChord = parse_key_event_to_chord(event);

// If on the settings screen:
client.write_typed(
    &oxpath!("ui", "_horns", "settings", "input", "key"),
    &chord,
).await?;
// horns' KeyDispatchSubscription fires; its cascade bumps the render
// tick; horns-ratatui's ViewRenderSubscription draws to the terminal.
```

On resize:

```rust
use horns::view::Area;  // add Area to horns_core::view if not present:
                         //   #[derive(Serialize,Deserialize,Clone,Debug,PartialEq)]
                         //   pub struct Area { pub w: u16, pub h: u16 }

client.write_typed(
    &oxpath!("ui", "_horns", "settings", "input", "area"),
    &Area { w, h },
).await?;
client.write_typed(
    &oxpath!("ui", "_horns", "settings", "render", "tick"),
    &(next_tick as u64),
).await?;
```

If `horns_core::view::Area` doesn't exist yet, add it to `crates/horns-core/src/view.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Area {
    pub w: u16,
    pub h: u16,
}
```

- [ ] **Step 4: Call `settings::install` at startup**

In `crates/ox-cli/src/main.rs` (or wherever `BrokerStore::new` is called and the event loop starts), after the broker is built and the `Terminal` is initialized but before the event loop begins:

```rust
use std::sync::Arc;
use parking_lot::Mutex;

let terminal_arc = Arc::new(Mutex::new(terminal));  // wherever `terminal` is constructed
let settings_handle = crate::settings::install(&mut broker, terminal_arc.clone()).await?;
// Keep `settings_handle` alive for the program's lifetime so its
// subscriptions stay registered.
```

The existing call sites that constructed a BindingRegistry/CommandRegistry/RendererRegistry directly are no longer needed — `settings::install` owns that wiring now. Find and remove them:

```bash
grep -rn "BindingRegistry::new\|CommandRegistry::new\|RendererRegistry::new" crates/ox-cli/src/
```

Each remaining call site should either be inside horns-core tests (keep) or inside the path moving to `settings::install` (remove or inline).

- [ ] **Step 5: Delete `crates/ox-cli/src/dispatch.rs`**

```bash
rm /Users/alex/Devel/AdjectiveNoun/ox/crates/ox-cli/src/dispatch.rs
```

In `crates/ox-cli/src/lib.rs` (or `main.rs`), remove `pub mod dispatch;`.

- [ ] **Step 6: Sweep for remaining `dispatch::send_key` callers**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
grep -rn "dispatch::send_key\|send_key(" crates/ox-cli/src/
```

For settings screen call sites: replace with broker writes (see step 3).
For inbox/thread/history call sites: **leave them**. Migration of those screens is out of scope; they retain their legacy dispatch path. The function they were calling (`crate::dispatch::send_key`) is gone, so they'll fail to compile — wrap them in a temporary in-file stub or move their dispatch routing to a new helper. The cleanest path: keep the legacy dispatch wrapper for non-settings screens in a small new file `crates/ox-cli/src/legacy_dispatch.rs`, copying just the inbox/thread/history branches of the old `send_key`.

- [ ] **Step 7: Build the workspace**

Run: `cargo build`
Expected: clean build. Errors here are highly informative — typically `Path` conversion issues, missing serde, or a binding constructor still mentioning `screen:`. Fix in place.

- [ ] **Step 8: Run the full test suite**

Run: `cargo test`
Expected: all tests pass. Settings integration tests now exercise the full mount lifecycle.

- [ ] **Step 9: Smoke-test the settings screen by hand**

```bash
cargo build -p ox-cli
```

Ask the user to run `./target/debug/ox settings` themselves and confirm:
- Navigation works (j/k cycle rows)
- Compose form opens (`a`), accepts text input, Esc cancels, Enter commits
- Delete confirm works (`d`, then `y`/`n`)
- Edit field inline works (printable ASCII inserts, Enter commits, Esc cancels)
- Save works (Ctrl+S)
- Connectivity test (`t`) and catalog refresh (`r`) work

Do not skip this step. The integration test suite doesn't replace human-eye verification for UX behavior. If anything is broken, fix and re-test.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
refactor: rewire event loop to broker writes; settings becomes horns mount

The event loop no longer calls dispatch::send_key. Instead it writes
KeyChord to the configured input path; horns' KeyDispatchSubscription
fires, runs the matched Command or KeyHandler, produces a cascade
ending in a render-tick bump; horns-ratatui's ViewRenderSubscription
draws the resulting View.

ox-cli/src/dispatch.rs is deleted. ox-cli/src/settings/mod.rs gains
a public install(broker, terminal) that wires the settings screen
as a horns instance at the horns/settings/* and ui/_horns/settings/*
broker prefixes. Inbox/thread/history retain their legacy dispatch
in a new legacy_dispatch helper until they too migrate.
CM
)"
```

---

## Task 11: Move the UI framework docs into crates/horns/docs/

**Files:**
- Move: `docs/ui_framework.md` → `crates/horns/docs/ui_framework.md`
- Move: `docs/ui_framework/architecture.md` → `crates/horns/docs/architecture.md`
- Move: `docs/ui_framework/howto.md` → `crates/horns/docs/howto.md`
- Move: `docs/ui_framework/reference.md` → `crates/horns/docs/reference.md`
- Delete: `docs/ui_framework/` (empty after the move)

- [ ] **Step 1: Move the docs with git history preservation**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
mkdir -p crates/horns/docs
git mv docs/ui_framework.md           crates/horns/docs/ui_framework.md
git mv docs/ui_framework/architecture.md crates/horns/docs/architecture.md
git mv docs/ui_framework/howto.md        crates/horns/docs/howto.md
git mv docs/ui_framework/reference.md    crates/horns/docs/reference.md
rmdir docs/ui_framework
```

- [ ] **Step 2: Verify git tracks the renames**

Run: `git status`
Expected: four `renamed:` entries. If they show as delete + new instead, that's still acceptable — git's blame and log -- follow can chase history either way.

- [ ] **Step 3: Update cross-doc links inside the moved files**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
grep -rn "docs/ui_framework" crates/horns/docs/
```

Replace any `docs/ui_framework/...` with relative links inside `crates/horns/docs/`. Most cross-doc references in the existing docs are already relative (e.g., `architecture.md`), so this may be a no-op.

- [ ] **Step 4: Update external references**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
grep -rn "docs/ui_framework" --include="*.md" --include="*.rs"
```

Update any reference from `docs/ui_framework` to `crates/horns/docs` in:
- READMEs
- Other specs in `docs/superpowers/`
- Doc comments inside source files

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: move ui_framework docs into crates/horns/docs/"
```

---

## Task 12: Sanitize the docs to describe horns generically

**Files:**
- Modify: `crates/horns/docs/ui_framework.md`
- Modify: `crates/horns/docs/architecture.md`
- Modify: `crates/horns/docs/howto.md`
- Modify: `crates/horns/docs/reference.md`

Read each file end-to-end before editing.

- [ ] **Step 1: Replace concrete settings paths with generic placeholders**

Apply across all four files (do NOT sed blindly — some references are legitimate worked-example illustrations of the settings screen). For each occurrence:

- `Screen::Settings` → `<your-screen>` (in prose) or remove from code blocks
- `ui/settings/focused` → `<cursor_path>` (the install option)
- `ui/settings/cursor` → `<cursor_path>` (collapsed into single concept)
- `settings/accounts/<name>` → `<page>/<row>` (in framework prose)
- `settings/_compose_form/...` → `<page>/_<widget>/...`
- `ui/settings/new_account/buffer` → `<ui-state-prefix>/<widget>/buffer`

Preserve the settings examples in the howto.md "worked example" section.

- [ ] **Step 2: Replace the "60-second pitch" in `ui_framework.md`**

Rewrite the top of `crates/horns/docs/ui_framework.md`:

```markdown
# horns: a path-MVU UI toolkit

horns is a reusable path-MVU UI framework. The settings screen in
ox-cli is its canonical first user; the inbox, threads, and history
screens will move onto it as they're rebuilt.

This index page is the only thing every reader needs. The rest is
split so you can read just what's relevant to your task.

## When to read what

| You are... | Read |
|---|---|
| First-time reader | This file (60 seconds), then `architecture.md` |
| Building a screen or shipping a widget | `howto.md` |
| Looking up a type signature, path, or filename | `reference.md` |
| Curious about why it's shaped this way | `architecture.md` §Why |

## 60-second pitch

Every piece of UI state lives at a path in StructFS. The framework
is installed as a broker mount — `horns::install(broker, options)`
registers three subscriptions (KeyDispatch, Render, ThemeChange)
that own the runtime. After install, the host's only interface is
broker writes: write a `KeyChord` to the configured input path; the
dispatcher fires; the matched command or handler produces writes;
the broker cascades them; the renderer fires; the backend draws.

Renderers are pure `&mut dyn Reader → View` functions, registered
against cursor paths. Commands and handlers are pure
`&mut dyn Reader → Vec<Write>` functions; the runtime is the only
place I/O happens. A write to a data-tree path IS the action that
path represents.

The View enum is small and curated. The translator (horns-ratatui's
`render_to_frame`) is total over it. Adding a "widget" requires
extending the enum *and* the translator — that cost is the point.
```

Keep the existing "Four architectural commitments" and "Seven
invariants you must keep" sections — they're still accurate. Replace
specific paths inside them with placeholders.

- [ ] **Step 3: Drop the stale Branch / SHA section**

In `crates/horns/docs/ui_framework.md`, remove the "Branch / SHA" section that says "Framework landed on branch `improvements`...". The extraction supersedes this lineage.

- [ ] **Step 4: Add new sections to `architecture.md`**

Insert near the top of `crates/horns/docs/architecture.md`, after the "Three trees" section, a "Mount, not library" section:

```markdown
## Mount, not library

horns is a *mount* on the broker, not a library you call. The host
calls `horns::install(broker, options)` exactly once per logical
screen at startup. After install, the host's only horns interface
is broker writes — write a `KeyChord` to the configured input path
and the rest of the framework runs reactively through subscriptions.

Three subscriptions own the runtime:

- `KeyDispatchSubscription` watches the input path. On every write,
  it computes the scope path from the cursor's ancestor chain and
  runs the matched command or handler.
- `RenderSubscription` watches a render-tick path that the dispatch
  subscription bumps after each cascade. On every write, it walks
  the renderer registry and writes the resulting View to the
  configured view-output path.
- `ThemeChangeSubscription` watches the theme path. On every write,
  it bumps the render tick so the screen re-renders with the new
  palette.

A backend (horns-ratatui) is also a mount: it registers its own
subscription watching the view-output path and draws each new View
to a terminal. horns and the backend communicate by the View schema
written to a broker path, not by Rust types — swap horns-ratatui for
horns-web (DOM patches) or horns-iced (native) without touching
horns-core.

### Multi-mount

`horns::install` can be called more than once at disjoint broker
prefixes. Each call produces an independent horns instance — its own
cursor path, its own input path, its own render output. A host with
multiple screens runs one install per screen and routes input by
writing to the corresponding input path.
```

After "Mount, not library", insert "Recursive composability":

```markdown
## Recursive composability

The cursor is one path. The scope path is `cursor.ancestors()`.
Bindings and handlers are keyed by exact paths. None of these scale
with nesting depth, so horns is recursively composable structurally,
without any framework-level "widget hierarchy" type.

A reusable sub-widget (date picker, file picker, command palette)
exports an install function:

```rust
pub fn install(
    namespace:  Path,                       // cursor namespace the widget owns
    ui_prefix:  Path,                       // working state subtree
    options:    /* widget-specific options */,
    bundle:     &mut HornsInstallBundle,    // mutable accumulator
);
```

The host calls `install` once per sub-widget at construction. The
widget adds its bindings, handlers, commands, renderers to the
bundle under paths it owns. The bundle is then passed to
`horns::install`. Multi-instance support is free — install the
same widget twice at different namespaces.

A sub-widget's bindings live in the parent's binding subtree but
are scoped to paths the parent doesn't otherwise use:

- **Not hidden:** the parent can enumerate every binding for
  help/audit/override.
- **Isolated:** sibling instances don't conflict because the cursor
  is one path; only one widget's namespace can be on its ancestry
  at a time.

### Phase semantics

Capture is outer→inner; Bubble is inner→outer. A parent that wants
to claim a key absolutely (regardless of nested widgets) registers
at Capture on its own scope. A parent that wants to claim a key
conditionally (only if no nested widget claimed it) registers at
Bubble. Each level chooses.
```

After "Recursive composability", insert "Opaque handlers vs introspectable bindings":

```markdown
## Opaque handlers vs introspectable bindings

The framework has two dispatch tiers at every scope+phase:

1. **Discrete bindings** (`BindingEntry`): introspectable. Lifecycle
   keys like Esc, Tab, Enter, Backspace, and named command keys go
   here. The help screen lists them; disk overrides edit them;
   accessibility audits enumerate them.
2. **Handlers** (`KeyHandler`): opaque. A handler is a closure that
   inspects the key and returns `Some(writes)` to claim or `None` to
   pass. Use for bulk consumption — a text field claiming any
   printable ASCII registers one handler, not 96 BindingEntries.

The dispatcher's per-phase walk asks discrete first (specificity-
ranked), then handlers (registration order). Discrete wins on tie at
the same scope+phase. The two tiers are deliberately separate; don't
unify them into a single `Matcher = Exact | Predicate` entry — the
introspection contract is the difference between "named, audible"
and "opaque, consumed."
```

- [ ] **Step 5: Add a "Shipping a reusable widget" section to howto.md**

Append a new section near the end of `crates/horns/docs/howto.md`:

```markdown
## Shipping a reusable widget

A reusable horns widget (date picker, file picker, command palette,
text field) ships as a crate that exports an `install` function. The
host calls it per widget at construction, passing the namespace and
UI-state prefix the widget should own.

```rust
// in my-cool-datepicker crate:
pub struct DatePickerOptions { /* ... */ }

pub fn install(
    namespace: structfs_core_store::Path,
    ui_prefix: structfs_core_store::Path,
    options:   DatePickerOptions,
    bundle:    &mut HornsInstallBundle,
) {
    // Register lifecycle bindings (introspectable):
    bundle.bindings.push((
        BindingId(format!("{}.cancel", ui_prefix.last_component())),
        BindingEntry {
            scope: BindingScope::Exact(namespace.clone()),
            key: /* Esc */,
            phase: Phase::Capture,
            command_id: CommandId(format!("{}.cancel", ui_prefix.last_component())),
        },
    ));

    // Register bulk-input handlers (opaque):
    let handler_id = HandlerId(format!("{}.keys", ui_prefix.last_component()));
    bundle.handlers.insert(handler_id.clone(), Arc::new(DatePickerKeyHandler { ui_prefix: ui_prefix.clone() }));
    bundle.handler_metadata.push((handler_id, HandlerMetadata {
        scope: BindingScope::Exact(namespace.join("field")),
        phase: Phase::Target,
        class: "datepicker_field".into(),
    }));

    // Register commands and renderers under the namespace too.
    bundle.commands.insert(/* ... */);
    bundle.renderers.insert(namespace.join("field"), Box::new(DatePickerRenderer { ui_prefix }));
}
```

The widget's bindings live in the parent's binding subtree but are
scoped to paths the parent doesn't use. Multi-instance support is
free — `install` twice with different namespaces.
```

- [ ] **Step 6: Update reference.md's API surface**

In `crates/horns/docs/reference.md`, replace the existing "Types" section's lead with a current public API listing:

```markdown
## Public API surface

```rust
// horns-core:
pub use install::{install, HornsHandle, InstallOptions};
pub use binding::{BindingEntry, BindingId, BindingRegistry, BindingScope, HandlerEntry, HandlerId, HandlerMetadata, KeyHandler, Phase};
pub use command::{Command, CommandCtx, CommandDisplay, CommandId, CommandMetadata, CommandRegistry, CommandScope};
pub use key::{KeyChord, KeyCodeRepr, KeyModifierSet};
pub use render::{AscendRule, Renderer, RenderCtx, RendererMetadata, RendererRegistry};
pub use view::View;
pub use write::Write;

// horns-ratatui:
pub use install::{install, RatatuiHandle, RatatuiOptions};
pub use render::render_to_frame;
pub use theme::Theme;
```
```

Update the file-map section (the tree of files under each crate) to match the post-extraction layout from the spec's "Crate topology" section.

Replace references to:
- `dispatch_settings_key(...)` → `horns::install(...)` (the public API; `Dispatcher` is internal)
- `crates/ox-cli/src/settings/binding_registry.rs` → `crates/horns-core/src/binding.rs`
- `crates/ox-cli/src/settings/command_registry.rs` → `crates/horns-core/src/command.rs`
- `crates/ox-cli/src/settings/registry.rs` → `crates/horns-core/src/render.rs`
- `crates/ox-cli/src/settings/dispatch.rs` → `crates/horns-core/src/dispatch.rs`
- `crates/ox-cli/src/view_render.rs` → `crates/horns-ratatui/src/render.rs`

- [ ] **Step 7: Verify the docs compile (in spirit)**

```bash
cd /Users/alex/Devel/AdjectiveNoun/ox
grep -rn "Screen::Settings\|dispatch_settings_key" crates/horns/docs/
```

Expected: empty result (or only inside the "settings as a worked example" section). Fix stragglers.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
docs: sanitize horns docs — generic prose, settings as worked example

Replaces settings-specific paths in framework prose with placeholder
notation. Adds new sections: 'Mount, not library', 'Recursive
composability', 'Opaque handlers vs introspectable bindings',
'Shipping a reusable widget'. Updates reference.md's public API
surface to match the post-extraction shape.
CM
)"
```

---

## Task 13: Performance benchmark — dispatch path

**Files:**
- Create: `crates/horns-core/benches/dispatch.rs`
- Modify: `crates/horns-core/Cargo.toml`

- [ ] **Step 1: Add criterion and bench manifest**

In `crates/horns-core/Cargo.toml`:

```toml
[dev-dependencies]
# existing entries...
criterion = "0.5"
ox-path = { path = "../ox-path" }
ox-store-util = { path = "../ox-store-util" }

[[bench]]
name = "dispatch"
harness = false
```

- [ ] **Step 2: Write the benchmark**

Create `crates/horns-core/benches/dispatch.rs`:

```rust
use criterion::{criterion_group, criterion_main, Criterion};
use horns_core::{
    BindingEntry, BindingRegistry, BindingScope, CommandRegistry, Dispatcher,
    KeyChord, KeyCodeRepr, KeyModifierSet, Phase, RendererRegistry,
};
use ox_path::oxpath;
use ox_store_util::local_config::LocalConfig;
use structfs_core_store::{Reader, Value};

fn build_settings_like_registry() -> BindingRegistry {
    let mut r = BindingRegistry::new();
    // Mirror the rough shape of the settings binding table — ~40
    // entries across various scopes and phases.
    for i in 0..40 {
        r.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "bench", format!("k{i}").as_str())),
            key: KeyChord { modifiers: KeyModifierSet::default(), code: KeyCodeRepr::Char('a') },
            phase: Phase::Target,
            command_id: horns_core::CommandId(format!("bench.{i}")),
        });
    }
    r
}

fn bench_dispatch(c: &mut Criterion) {
    let bindings = build_settings_like_registry();
    let commands = CommandRegistry::new();
    let renderers = RendererRegistry::new();

    let mut snap = LocalConfig::new();
    snap.set("ui/settings/focused", Value::String("settings/bench/k20".into()));

    let dispatcher = Dispatcher::new(oxpath!("ui", "settings", "focused"));
    let chord = KeyChord { modifiers: KeyModifierSet::default(), code: KeyCodeRepr::Char('a') };

    c.bench_function("dispatch_no_match_no_command", |b| {
        b.iter(|| {
            let _ = dispatcher.dispatch(
                &mut snap as &mut dyn Reader,
                &chord,
                &bindings,
                &commands,
                &renderers,
            );
        });
    });
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
```

- [ ] **Step 3: Run the benchmark**

Run: `cargo bench -p horns-core --bench dispatch`
Expected: criterion output with a time/iter in the microsecond range. Record the number — note it in the benchmark file's header comment or in `crates/horns/docs/reference.md` under a "Performance" heading.

Note: this measures the dispatcher in isolation. The subscription cascade adds broker dispatch overhead — measure that separately if cascade latency turns out to feel slow during human use of the settings screen (Task 10 step 9).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "$(cat <<'CM'
test: dispatch microbenchmark

A criterion benchmark for the dispatcher's per-keystroke cost with
a representative binding table (~40 entries). Doesn't measure the
broker subscription cascade latency; that's a separate concern
warranting its own benchmark if perceived latency surfaces in
human use.
CM
)"
```

---

## After all 13 tasks land

- The horns extraction is complete; the settings screen is a clean horns instance.
- `ox-cli/src/settings/` contains only settings-specific code.
- `ox-cli/src/event_loop.rs` imports no horns types except path constants and `KeyChord`/`Area` from the umbrella.
- The text-input scope is one `TextInputHandler` + 4 discrete lifecycle bindings (no longer 96 entries).
- `crates/horns/docs/` describes the framework generically with settings as a worked example.

### Out-of-scope follow-ups (not in this plan)

- Migrate inbox/threads/history screens to horns instances.
- Spin out `structfs-broker-traits` to decouple horns-core from ox-broker.
- Ship a second backend (horns-web, horns-iced) to validate backend pluggability.
- Build a live keybinding editor that writes to the bindings subtree.
- Hot-reload of Commands/Renderers (mutate side-tables at runtime).
- Migrate `RenderCtx::theme` from `&dyn Any` to a typed `Theme` once one is canonicalized.

## Self-review (performed by the author)

- **Spec coverage:** every spec section maps to a task. Mount API → Task 8. Screen/Mode removal → Tasks 3–4. KeyHandler tier → Tasks 7 + 9. Doc move + sanitize → Tasks 11 + 12. Migration plan → Tasks 1–10 + 13.
- **Placeholder scan:** no `TBD`, `???`, or `fill in details`. Several `todo!()` macros remain in Task 8 step 3 with explicit "drive to green via test in step 6/8" guidance pointing at the canonical broker subscription shape.
- **Type consistency:** every type used in later tasks is defined in an earlier task. `BindingEntry` (Tasks 3, 4); `KeyHandler`, `HandlerEntry`, `HandlerMetadata`, `HandlerId` (Task 7); `Dispatcher` (Task 5); `InstallOptions`, `HornsHandle` (Task 8); `TextInputHandler` (Task 9); `RatatuiOptions`, `RatatuiHandle` (Task 8 step 10). Field names like `cursor_path`, `bindings_prefix`, `handler_metadata` are used consistently.
