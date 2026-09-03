# horns Component Nesting (NamespaceView + Mount Tables) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add horns-level primitives for nesting horns instances inside one another, with mount-table-driven path binding between components, full install/uninstall lifecycle, recursive composition, and auto-wired coarse reactivity — without modifying structfs.

**Architecture:** Path rewriting happens at the horns boundary via two wrappers — `NamespaceView` (Reader) and `rewrite_writes` (write batch transformer) — driven by a `MountTable`. Each component install carries an `InstallerScope` that represents the parent shell's effective namespace + resolved mount table; child installs resolve their mount physical paths *through* the parent scope, so the same component declaration works at any nesting depth. `install_component` returns an `InstallHandle` that carries subscription ids for clean teardown via `uninstall_component`. Coarse per-component reactivity (a single tick-bump subscription watching the component's namespace prefix and each resolved mount's physical path) is auto-wired by default, with a per-install opt-out and a self-write guard to prevent double-renders.

**Tech Stack:**
- Rust 2024 edition
- `horns-core` (this crate) — all new code lives here
- `structfs-core-store` traits (`Reader`, `Writer`, `Record`, `Value`, `Path`) — used unchanged
- `horns-core::subscription` (`Subscription`, `SubCtx`, `PathPattern`, `SubscriptionId`) — used unchanged
- `serde` + `structfs-serde-store` for metadata roundtrip
- `ox-broker` (dev-dep only, for end-to-end tests)
- `ox_path::oxpath!` macro for tests

---

## File Structure

New files under `crates/horns-core/src/`:

- `mount.rs` — `Mount`, `MountAccess`, `MountTable`, `MountError`, `InstallerScope` types; resolution and composition logic; the shared `rewrite_path` helper used by both `namespace.rs` and `component.rs`.
- `namespace.rs` — `NamespaceView<'a>` Reader wrapper, `rewrite_writes` helper, `RewritingSubscription`, `NamespaceError`.
- `tracked.rs` — `TrackedReader<'a>` Reader wrapper that records reads (kept available for future fine-grained reactivity; not wired to runtime in this plan).
- `component.rs` — `ComponentSpec`, `ComponentInstall`, `ComponentInstallFn`, `install_component`, `uninstall_component`, `InstallHandle`, `ComponentReactivitySubscription`, the `ReactivityMode` opt-out enum, and the top-of-file author-facing doc block.

Modified files:

- `crates/horns-core/src/lib.rs` — declare and re-export the new modules.
- `crates/horns-core/tests/component_nesting_e2e.rs` (new) — end-to-end tests using `ox-broker::BrokerStore` + an in-test `MemoryStore`, modeled on `install_e2e.rs`. Includes BOTH a one-level case that exercises `RewritingSubscription` through the broker AND a two-level recursive case.

No changes to `structfs`. No changes to `ox-broker`. No changes to existing `horns-core` modules other than `lib.rs`.

---

## Conventions Used Throughout

- Every TDD cycle: write failing test → run and confirm fail → implement → run and confirm pass → commit.
- Run a single test with: `cargo test -p horns-core <test_name> -- --nocapture`.
- Run the whole horns-core suite with: `cargo test -p horns-core`.
- The component's render tick path convention: `<effective-namespace>/render_tick`. The component install API surfaces this so the trigger subscription knows where to write.
- All paths used in tests come from the `ox_path::oxpath!` macro (already a dev-dep of horns-core via the dispatcher tests).
- **Subscription registration order** is significant when one subscription's output feeds another. Tests that depend on ordering register the producing subscription before the consuming one; this matches the pattern in `crates/ox-cli/src/settings/mod.rs` (ShortcutResolver before render).

---

### Task 1: Mount types, MountTable, InstallerScope, rewrite_path

**Files:**
- Create: `crates/horns-core/src/mount.rs`
- Modify: `crates/horns-core/src/lib.rs`

- [ ] **Step 1.1: Write failing tests for Mount/MountTable resolve and validate**

Create `crates/horns-core/src/mount.rs`:

```rust
//! Mount table types and resolution. A `Mount` declares that a
//! component-local path is bound to a physical broker path. The
//! `MountTable` is consulted by `NamespaceView` and `rewrite_writes`
//! to translate component-local paths into physical broker paths.
//!
//! `InstallerScope` represents an installer's effective namespace +
//! resolved mount table, which is what child installs resolve their
//! own mount physical paths against. Composing scopes is what makes
//! recursive nesting work: a sub-shell can install components whose
//! mount physical paths reference the sub-shell's own locals or
//! mounted aliases, and those references resolve correctly to the
//! ultimate broker paths.

use serde::{Deserialize, Serialize};
use structfs_core_store::Path;

use crate::path_serde;

/// Access mode for a mount. Determines whether the component-side
/// surface is allowed to read, write, or both at the mounted path.
///
/// - `Shared`: full read/write.
/// - `View`: read-only on the component side. Writes to the local path
///   are rejected by the framework.
/// - `Output`: write-only on the component side. Reads from the local
///   path are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountAccess {
    Shared,
    View,
    Output,
}

/// One mount entry: the component-local path `local` is bound to the
/// physical broker path `physical`, with the given access mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    #[serde(with = "path_serde")]
    pub local: Path,
    #[serde(with = "path_serde")]
    pub physical: Path,
    pub access: MountAccess,
}

/// A collection of `Mount` entries. Order of entries is preserved; the
/// first matching entry wins on resolve (documented behavior — duplicate
/// locals are forbidden by `validate`, but in tests / debug surfaces
/// the order is still meaningful).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountTable {
    entries: Vec<Mount>,
}

impl MountTable {
    /// Return the physical broker path for `local`, if `local` exactly
    /// matches the `local` field of a mount entry.
    pub fn resolve(&self, local: &Path) -> Option<&Mount> {
        self.entries.iter().find(|m| m.local.components == local.components)
    }

    /// All entries in registration order.
    pub fn entries(&self) -> &[Mount] {
        &self.entries
    }

    /// Check the table for structural problems. Returns `Ok(())` if the
    /// table is well-formed.
    pub fn validate(&self) -> Result<(), MountError> {
        for (i, a) in self.entries.iter().enumerate() {
            for b in &self.entries[i + 1..] {
                if a.local.components == b.local.components {
                    return Err(MountError::DuplicateLocal(a.local.clone()));
                }
            }
        }
        Ok(())
    }
}

impl From<Vec<Mount>> for MountTable {
    fn from(entries: Vec<Mount>) -> Self {
        Self { entries }
    }
}

/// Errors returned by `MountTable::validate` and the install pipeline.
#[derive(Debug, PartialEq, Eq)]
pub enum MountError {
    /// Two mount entries declared the same `local` path.
    DuplicateLocal(Path),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_path::oxpath;

    #[test]
    fn resolve_returns_entry_for_mounted_local() {
        let table = MountTable::from(vec![Mount {
            local: oxpath!("theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        }]);
        let entry = table.resolve(&oxpath!("theme")).expect("resolves");
        assert_eq!(entry.physical, oxpath!("ui", "theme"));
        assert_eq!(entry.access, MountAccess::Shared);
    }

    #[test]
    fn resolve_returns_none_for_unmounted_local() {
        let table = MountTable::from(vec![Mount {
            local: oxpath!("theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        }]);
        assert!(table.resolve(&oxpath!("buffer")).is_none());
    }

    #[test]
    fn validate_rejects_duplicate_local_paths() {
        let table = MountTable::from(vec![
            Mount {
                local: oxpath!("theme"),
                physical: oxpath!("ui", "theme_a"),
                access: MountAccess::Shared,
            },
            Mount {
                local: oxpath!("theme"),
                physical: oxpath!("ui", "theme_b"),
                access: MountAccess::Shared,
            },
        ]);
        assert!(matches!(table.validate(), Err(MountError::DuplicateLocal(_))));
    }

    #[test]
    fn validate_accepts_distinct_locals_pointing_at_same_physical() {
        let table = MountTable::from(vec![
            Mount {
                local: oxpath!("primary_theme"),
                physical: oxpath!("ui", "theme"),
                access: MountAccess::Shared,
            },
            Mount {
                local: oxpath!("alias_theme"),
                physical: oxpath!("ui", "theme"),
                access: MountAccess::Shared,
            },
        ]);
        assert!(table.validate().is_ok());
    }
}
```

Wire the module in `crates/horns-core/src/lib.rs`:

```rust
// add near `pub mod install;`
pub mod mount;
```

- [ ] **Step 1.2: Run tests to verify they pass**

Run: `cargo test -p horns-core mount`
Expected: 4 passing.

- [ ] **Step 1.3: Add failing tests for rewrite_path and InstallerScope**

Append to the `tests` module:

```rust
#[test]
fn rewrite_path_prepends_namespace_when_unmounted() {
    let table = MountTable::default();
    let ns = oxpath!("components", "panel_a");
    let out = rewrite_path(&oxpath!("buffer"), &table, &ns);
    assert_eq!(out, oxpath!("components", "panel_a", "buffer"));
}

#[test]
fn rewrite_path_uses_physical_when_mounted() {
    let table = MountTable::from(vec![Mount {
        local: oxpath!("theme"),
        physical: oxpath!("ui", "theme"),
        access: MountAccess::Shared,
    }]);
    let ns = oxpath!("components", "panel_a");
    let out = rewrite_path(&oxpath!("theme"), &table, &ns);
    assert_eq!(out, oxpath!("ui", "theme"));
}

#[test]
fn installer_scope_root_resolves_to_input_paths() {
    let scope = InstallerScope::root();
    // At root, a path with no mounts resolves to itself (no prepending
    // because the namespace is empty).
    let out = scope.resolve(&oxpath!("ui", "theme"));
    assert_eq!(out, oxpath!("ui", "theme"));
}

#[test]
fn installer_scope_with_namespace_prepends_for_unmounted() {
    let scope = InstallerScope::new(
        oxpath!("app", "screens", "settings"),
        MountTable::default(),
    );
    let out = scope.resolve(&oxpath!("ui", "theme"));
    assert_eq!(out, oxpath!("app", "screens", "settings", "ui", "theme"));
}

#[test]
fn installer_scope_resolves_through_own_mounts_first() {
    let scope = InstallerScope::new(
        oxpath!("sub"),
        MountTable::from(vec![Mount {
            local: oxpath!("inherited_theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        }]),
    );
    // "inherited_theme" matches the mount, so resolution uses physical
    // (NOT the sub-namespace prefix path).
    let out = scope.resolve(&oxpath!("inherited_theme"));
    assert_eq!(out, oxpath!("ui", "theme"));
}

#[test]
fn child_mount_physical_resolves_through_parent_scope() {
    // Parent scope: namespace `sub`, with an "inherited_theme" mount
    // pointing at `ui/theme`.
    let parent = InstallerScope::new(
        oxpath!("sub"),
        MountTable::from(vec![Mount {
            local: oxpath!("inherited_theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        }]),
    );
    // Child declares its own mount referencing "inherited_theme" in
    // parent scope.
    let child_mount = Mount {
        local: oxpath!("theme"),
        physical: oxpath!("inherited_theme"),
        access: MountAccess::Shared,
    };
    let resolved = resolve_child_mount(&child_mount, &parent);
    // Resolution: "inherited_theme" in parent's scope resolves to
    // "ui/theme" (via parent's mount). That becomes child's physical.
    assert_eq!(resolved.physical, oxpath!("ui", "theme"));
    // Local and access unchanged.
    assert_eq!(resolved.local, child_mount.local);
    assert_eq!(resolved.access, child_mount.access);
}
```

- [ ] **Step 1.4: Run tests to verify they fail**

Run: `cargo test -p horns-core mount`
Expected: FAIL with "cannot find function `rewrite_path` / `resolve_child_mount`" / "cannot find type `InstallerScope`".

- [ ] **Step 1.5: Implement rewrite_path, InstallerScope, resolve_child_mount**

Add to `crates/horns-core/src/mount.rs` above the `#[cfg(test)]` block:

```rust
/// Rewrite a single component-local path to its broker path under the
/// given mount table and namespace.
///
/// Resolution order:
/// 1. If `local` matches a mount entry, use that mount's physical path
///    verbatim.
/// 2. Otherwise, prepend the namespace prefix.
///
/// Shared helper used by `namespace.rs` (Reader rewriting) and
/// `component.rs` (metadata path rewriting). Keep these two callers in
/// sync via this function — do not re-implement the rule in either
/// caller.
pub(crate) fn rewrite_path(local: &Path, table: &MountTable, namespace: &Path) -> Path {
    if let Some(entry) = table.resolve(local) {
        return entry.physical.clone();
    }
    let mut components = namespace.components.clone();
    components.extend(local.components.iter().cloned());
    Path::try_from_components(components).unwrap_or_else(|_| local.clone())
}

/// An installer's effective scope: the namespace it lives under at the
/// broker, plus the mount table it received from its own parent. Child
/// installs resolve their mount physical paths through this scope to
/// compute the broker paths they should ultimately address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallerScope {
    namespace: Path,
    mounts: MountTable,
}

impl InstallerScope {
    /// Scope used at the very top of an install tree: empty namespace,
    /// no mounts.
    pub fn root() -> Self {
        Self {
            namespace: Path::try_from_components(Vec::<String>::new()).unwrap(),
            mounts: MountTable::default(),
        }
    }

    pub fn new(namespace: Path, mounts: MountTable) -> Self {
        Self { namespace, mounts }
    }

    pub fn namespace(&self) -> &Path {
        &self.namespace
    }

    pub fn mounts(&self) -> &MountTable {
        &self.mounts
    }

    /// Resolve a path expressed in this scope's local terms to its
    /// broker path. Equivalent to `rewrite_path` with this scope's
    /// mount table and namespace.
    pub fn resolve(&self, local: &Path) -> Path {
        rewrite_path(local, &self.mounts, &self.namespace)
    }
}

/// Resolve a child's mount physical path through the parent's scope,
/// producing a Mount whose `physical` field is the actual broker path.
/// `local` and `access` are passed through unchanged. This is what makes
/// recursive nesting work: the child can reference the parent's locals
/// in its mount declarations and the framework rewrites them to the
/// real broker paths during install.
pub(crate) fn resolve_child_mount(child: &Mount, parent: &InstallerScope) -> Mount {
    Mount {
        local: child.local.clone(),
        physical: parent.resolve(&child.physical),
        access: child.access,
    }
}
```

- [ ] **Step 1.6: Run tests to verify they pass**

Run: `cargo test -p horns-core mount`
Expected: 10 passing.

- [ ] **Step 1.7: Re-export public items from lib.rs**

Add to `crates/horns-core/src/lib.rs`:

```rust
pub use mount::{InstallerScope, Mount, MountAccess, MountError, MountTable};
```

Run: `cargo build -p horns-core`.

- [ ] **Step 1.8: Commit**

```bash
git add crates/horns-core/src/mount.rs crates/horns-core/src/lib.rs
git commit -m "feat(horns-core): add Mount, MountTable, InstallerScope, rewrite_path for component nesting"
```

---

### Task 2: NamespaceView Reader wrapper

**Files:**
- Create: `crates/horns-core/src/namespace.rs`
- Modify: `crates/horns-core/src/lib.rs`

- [ ] **Step 2.1: Write failing tests for read rewriting**

Create `crates/horns-core/src/namespace.rs`:

```rust
//! `NamespaceView` is the horns-side path-rewriting boundary for nested
//! components. It wraps a `Reader` together with a `MountTable` and a
//! namespace prefix; reads through it transparently rewrite paths so
//! component code can address its private namespace and mounted
//! aliases without ever naming the underlying physical paths.
//!
//! Write rewriting is exposed through the free function `rewrite_writes`,
//! which transforms a `Vec<Write>` returned from a command or
//! subscription handler through the same mount table. Errors propagate
//! to the caller — silent drops are not appropriate at this boundary
//! because a violated access mode is always a programmer error.

use std::collections::HashMap;

use structfs_core_store::{Error, Path, Reader, Record};

use crate::mount::{MountAccess, MountTable, rewrite_path};
use crate::write::Write;

/// Errors returned by `rewrite_writes` and `NamespaceView` when an
/// operation violates a mount access constraint.
#[derive(Debug, PartialEq, Eq)]
pub enum NamespaceError {
    /// A write was attempted on a path bound by a `MountAccess::View`
    /// mount (read-only on the component side).
    WriteDenied(Path),
    /// A read was attempted on a path bound by a `MountAccess::Output`
    /// mount (write-only on the component side).
    ReadDenied(Path),
}

/// Reader wrapper that rewrites paths through a `MountTable` and a
/// namespace prefix before delegating to the inner reader.
///
/// Reads of paths bound by `MountAccess::Output` mounts return
/// `Err(NamespaceError::ReadDenied)` encoded as a `structfs_core_store::Error`
/// so the wrapper preserves the `Reader` trait signature without
/// inventing a parallel error type. Callers that need to distinguish
/// can downcast on the message; the framework treats both as "this
/// path is unreadable in this context."
pub struct NamespaceView<'a> {
    inner: &'a mut dyn Reader,
    table: &'a MountTable,
    namespace: &'a Path,
}

impl<'a> NamespaceView<'a> {
    pub fn new(
        inner: &'a mut dyn Reader,
        table: &'a MountTable,
        namespace: &'a Path,
    ) -> Self {
        Self { inner, table, namespace }
    }
}

impl<'a> Reader for NamespaceView<'a> {
    fn read(&mut self, p: &Path) -> Result<Option<Record>, Error> {
        if let Some(entry) = self.table.resolve(p) {
            if matches!(entry.access, MountAccess::Output) {
                return Err(Error::store(
                    "namespace_view",
                    "read",
                    format!("read denied for output-only mount at {p}"),
                ));
            }
        }
        let physical = rewrite_path(p, self.table, self.namespace);
        self.inner.read(&physical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount::{Mount, MountAccess, MountTable};
    use ox_path::oxpath;
    use structfs_core_store::{Record, Value};

    /// Minimal in-process Reader used to drive the rewriter tests.
    #[derive(Default)]
    struct MapReader {
        records: HashMap<Vec<String>, Record>,
    }

    impl MapReader {
        fn insert(&mut self, p: &Path, r: Record) {
            self.records.insert(p.components.clone(), r);
        }
    }

    impl Reader for MapReader {
        fn read(&mut self, p: &Path) -> Result<Option<Record>, Error> {
            Ok(self.records.get(&p.components).cloned())
        }
    }

    fn s(v: &str) -> Record {
        Record::parsed(Value::String(v.into()))
    }

    #[test]
    fn read_unmounted_local_prepends_namespace() {
        let mut inner = MapReader::default();
        inner.insert(&oxpath!("components", "panel_a", "buffer"), s("hello"));

        let table = MountTable::default();
        let ns = oxpath!("components", "panel_a");
        let mut view = NamespaceView::new(&mut inner, &table, &ns);

        let r = view.read(&oxpath!("buffer")).unwrap();
        assert_eq!(r, Some(s("hello")));
    }

    #[test]
    fn read_mounted_local_uses_physical_path() {
        let mut inner = MapReader::default();
        inner.insert(&oxpath!("ui", "theme"), s("dark"));

        let table = MountTable::from(vec![Mount {
            local: oxpath!("theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        }]);
        let ns = oxpath!("components", "panel_a");
        let mut view = NamespaceView::new(&mut inner, &table, &ns);

        let r = view.read(&oxpath!("theme")).unwrap();
        assert_eq!(r, Some(s("dark")));
    }

    #[test]
    fn read_of_output_mount_returns_error() {
        let mut inner = MapReader::default();
        inner.insert(&oxpath!("ui", "panel_a_out"), s("published"));

        let table = MountTable::from(vec![Mount {
            local: oxpath!("output"),
            physical: oxpath!("ui", "panel_a_out"),
            access: MountAccess::Output,
        }]);
        let ns = oxpath!("components", "panel_a");
        let mut view = NamespaceView::new(&mut inner, &table, &ns);

        assert!(view.read(&oxpath!("output")).is_err());
    }
}
```

Add the module to `crates/horns-core/src/lib.rs`:

```rust
pub mod namespace;
pub use namespace::{NamespaceError, NamespaceView};
```

- [ ] **Step 2.2: Run tests to verify they pass**

Run: `cargo test -p horns-core namespace`
Expected: 3 passing.

- [ ] **Step 2.3: Commit**

```bash
git add crates/horns-core/src/namespace.rs crates/horns-core/src/lib.rs
git commit -m "feat(horns-core): add NamespaceView Reader wrapper that rewrites paths through mount table"
```

---

### Task 3: rewrite_writes helper

**Files:**
- Modify: `crates/horns-core/src/namespace.rs`
- Modify: `crates/horns-core/src/lib.rs`

- [ ] **Step 3.1: Write failing tests for write rewriting**

Append to the `tests` module in `crates/horns-core/src/namespace.rs`:

```rust
#[test]
fn rewrite_writes_prepends_namespace_for_unmounted_path() {
    let table = MountTable::default();
    let ns = oxpath!("components", "panel_a");
    let writes = vec![Write {
        path: oxpath!("buffer"),
        record: s("hello"),
    }];

    let result = rewrite_writes(writes, &table, &ns).expect("write rewriting");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].path, oxpath!("components", "panel_a", "buffer"));
}

#[test]
fn rewrite_writes_uses_physical_for_mounted_path() {
    let table = MountTable::from(vec![Mount {
        local: oxpath!("output"),
        physical: oxpath!("ui", "panel_a_out"),
        access: MountAccess::Output,
    }]);
    let ns = oxpath!("components", "panel_a");
    let writes = vec![Write {
        path: oxpath!("output"),
        record: s("published"),
    }];

    let result = rewrite_writes(writes, &table, &ns).expect("write rewriting");

    assert_eq!(result[0].path, oxpath!("ui", "panel_a_out"));
}

#[test]
fn rewrite_writes_rejects_write_to_view_mount() {
    let table = MountTable::from(vec![Mount {
        local: oxpath!("locale"),
        physical: oxpath!("ui", "locale"),
        access: MountAccess::View,
    }]);
    let ns = oxpath!("components", "panel_a");
    let writes = vec![Write {
        path: oxpath!("locale"),
        record: s("en"),
    }];

    let result = rewrite_writes(writes, &table, &ns);
    assert!(matches!(result, Err(NamespaceError::WriteDenied(_))));
}
```

- [ ] **Step 3.2: Run tests to verify they fail**

Run: `cargo test -p horns-core namespace`
Expected: FAIL with "cannot find function `rewrite_writes`".

- [ ] **Step 3.3: Implement rewrite_writes**

Add to `crates/horns-core/src/namespace.rs` above the `#[cfg(test)]` block:

```rust
/// Rewrite every path in `writes` through the mount table and namespace,
/// returning the physical writes the broker should see.
///
/// Returns `Err(NamespaceError::WriteDenied)` on the first write whose
/// local path is bound by a read-only mount. Partial-batch application
/// is not supported and would be misleading — the caller should treat
/// the whole batch as failed.
pub fn rewrite_writes(
    writes: Vec<Write>,
    table: &MountTable,
    namespace: &Path,
) -> Result<Vec<Write>, NamespaceError> {
    let mut out = Vec::with_capacity(writes.len());
    for w in writes {
        if let Some(entry) = table.resolve(&w.path) {
            if matches!(entry.access, MountAccess::View) {
                return Err(NamespaceError::WriteDenied(w.path));
            }
        }
        let physical = rewrite_path(&w.path, table, namespace);
        out.push(Write { path: physical, record: w.record });
    }
    Ok(out)
}
```

- [ ] **Step 3.4: Run tests to verify they pass**

Run: `cargo test -p horns-core namespace`
Expected: 6 passing.

- [ ] **Step 3.5: Re-export rewrite_writes**

Update the namespace re-export line in `crates/horns-core/src/lib.rs`:

```rust
pub use namespace::{NamespaceError, NamespaceView, rewrite_writes};
```

Run: `cargo build -p horns-core`.

- [ ] **Step 3.6: Commit**

```bash
git add crates/horns-core/src/namespace.rs crates/horns-core/src/lib.rs
git commit -m "feat(horns-core): add rewrite_writes for component-to-physical write translation"
```

---

### Task 4: TrackedReader wrapper

**Files:**
- Create: `crates/horns-core/src/tracked.rs`
- Modify: `crates/horns-core/src/lib.rs`

- [ ] **Step 4.1: Write the module with tests**

Create `crates/horns-core/src/tracked.rs`:

```rust
//! `TrackedReader` records every path it observes a read of, deduping
//! by exact path. It is the runtime-side primitive the framework will
//! use to drive fine-grained reactivity: after a renderer (or any
//! consumer) runs against a `TrackedReader`, `into_deps` returns the
//! set of physical paths that consumer depends on.
//!
//! This module ships the data structure but does **not** wire it to
//! subscription registration. The coarse reactivity subscription in
//! `component.rs` is sufficient for v1; the tracked reader is
//! available for a follow-up plan that turns reads into individual
//! exact-path subscriptions.

use std::collections::HashSet;

use structfs_core_store::{Error, Path, Reader, Record};

pub struct TrackedReader<'a> {
    inner: &'a mut dyn Reader,
    deps: HashSet<Vec<String>>,
}

impl<'a> TrackedReader<'a> {
    pub fn new(inner: &'a mut dyn Reader) -> Self {
        Self { inner, deps: HashSet::new() }
    }

    /// Consume the reader and return the set of paths it recorded
    /// reads of, deduped by component sequence.
    pub fn into_deps(self) -> HashSet<Path> {
        self.deps
            .into_iter()
            .filter_map(|comps| Path::try_from_components(comps).ok())
            .collect()
    }
}

impl<'a> Reader for TrackedReader<'a> {
    fn read(&mut self, p: &Path) -> Result<Option<Record>, Error> {
        self.deps.insert(p.components.clone());
        self.inner.read(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use ox_path::oxpath;
    use structfs_core_store::{Record, Value};

    #[derive(Default)]
    struct MapReader {
        records: HashMap<Vec<String>, Record>,
    }

    impl Reader for MapReader {
        fn read(&mut self, p: &Path) -> Result<Option<Record>, Error> {
            Ok(self.records.get(&p.components).cloned())
        }
    }

    #[test]
    fn records_paths_read() {
        let mut inner = MapReader::default();
        inner.records.insert(
            oxpath!("a", "b").components,
            Record::parsed(Value::String("x".into())),
        );

        let mut tracked = TrackedReader::new(&mut inner);
        let _ = tracked.read(&oxpath!("a", "b"));
        let _ = tracked.read(&oxpath!("c"));

        let deps = tracked.into_deps();
        assert!(deps.contains(&oxpath!("a", "b")));
        assert!(deps.contains(&oxpath!("c")));
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn dedupes_repeated_reads_of_same_path() {
        let mut inner = MapReader::default();
        let mut tracked = TrackedReader::new(&mut inner);

        let _ = tracked.read(&oxpath!("a"));
        let _ = tracked.read(&oxpath!("a"));
        let _ = tracked.read(&oxpath!("a"));

        let deps = tracked.into_deps();
        assert_eq!(deps.len(), 1);
        assert!(deps.contains(&oxpath!("a")));
    }
}
```

Add the module to `crates/horns-core/src/lib.rs`:

```rust
pub mod tracked;
pub use tracked::TrackedReader;
```

- [ ] **Step 4.2: Run tests to verify they pass**

Run: `cargo test -p horns-core tracked`
Expected: 2 passing.

- [ ] **Step 4.3: Commit**

```bash
git add crates/horns-core/src/tracked.rs crates/horns-core/src/lib.rs
git commit -m "feat(horns-core): add TrackedReader for future fine-grained reactivity"
```

---

### Task 5: RewritingSubscription

**Files:**
- Modify: `crates/horns-core/src/namespace.rs`
- Modify: `crates/horns-core/src/lib.rs`

- [ ] **Step 5.1: Write failing test**

Append to the `tests` module in `crates/horns-core/src/namespace.rs`:

```rust
#[test]
fn rewriting_subscription_translates_watches_and_returned_writes() {
    use std::sync::Arc;

    use crate::subscription::{
        AsyncWriter, BoxFuture, PathChange, PathPattern, SpawnHandle, SubCtx,
        Subscription, SubscriptionId,
    };

    // Inner subscription: watches a component-local path; on fire,
    // returns a write to its component-local output path.
    struct InnerSub {
        id: SubscriptionId,
        watches: Vec<PathPattern>,
    }

    impl Subscription for InnerSub {
        fn id(&self) -> &SubscriptionId { &self.id }
        fn watches(&self) -> &[PathPattern] { &self.watches }
        fn handle(&self, _: SubCtx<'_>) -> Vec<Write> {
            vec![Write {
                path: oxpath!("output"),
                record: s("from_inner"),
            }]
        }
    }

    let inner = Arc::new(InnerSub {
        id: SubscriptionId("inner".to_string()),
        watches: vec![PathPattern::Exact(oxpath!("buffer"))],
    });

    let table = MountTable::from(vec![Mount {
        local: oxpath!("output"),
        physical: oxpath!("ui", "panel_a_out"),
        access: MountAccess::Output,
    }]);
    let ns = oxpath!("components", "panel_a");
    let wrap = RewritingSubscription::new(
        inner,
        table.clone(),
        ns.clone(),
        SubscriptionId("wrap".to_string()),
    );

    // watches() reports rewritten patterns.
    assert_eq!(
        wrap.watches(),
        &[PathPattern::Exact(oxpath!("components", "panel_a", "buffer"))]
    );

    // handle() rewrites the returned write through the mount table.
    let mut snap = MapReader::default();
    let change = PathChange {
        path: oxpath!("components", "panel_a", "buffer"),
        before: None,
        after: Some(s("x")),
    };

    struct NoSpawn;
    impl SpawnHandle for NoSpawn {
        fn spawn(&self, _: BoxFuture<()>) -> tokio::task::AbortHandle {
            unreachable!("test does not exercise spawn");
        }
    }

    struct NoWriter;
    impl AsyncWriter for NoWriter {
        fn write(
            &self,
            _: Path,
            _: Record,
        ) -> BoxFuture<Result<Path, Error>> {
            unreachable!("test does not exercise async writer");
        }
    }

    let writer: Arc<dyn AsyncWriter> = Arc::new(NoWriter);
    let ctx = SubCtx {
        snapshot: &mut snap,
        change: &change,
        spawn: &NoSpawn,
        writer,
    };

    let writes = wrap.handle(ctx);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "panel_a_out"));
}

#[test]
fn rewriting_subscription_emits_error_event_on_view_mount_write() {
    // A subscription that tries to write to a View-mode mount is a
    // programmer error. The wrapper drops the offending batch AND
    // emits a structured `NamespaceErrorEvent` at the conventional
    // error-report path under the component's namespace so the
    // failure is observable from the broker — not just from tracing
    // logs.
    use std::sync::Arc;

    use crate::subscription::{
        AsyncWriter, BoxFuture, PathChange, PathPattern, SpawnHandle, SubCtx,
        Subscription, SubscriptionId,
    };

    struct InnerSub {
        id: SubscriptionId,
        watches: Vec<PathPattern>,
    }

    impl Subscription for InnerSub {
        fn id(&self) -> &SubscriptionId { &self.id }
        fn watches(&self) -> &[PathPattern] { &self.watches }
        fn handle(&self, _: SubCtx<'_>) -> Vec<Write> {
            vec![Write {
                path: oxpath!("locale"),  // View-mode mount — write denied
                record: s("en"),
            }]
        }
    }

    let inner = Arc::new(InnerSub {
        id: SubscriptionId("inner".to_string()),
        watches: vec![PathPattern::Exact(oxpath!("trigger"))],
    });

    let table = MountTable::from(vec![Mount {
        local: oxpath!("locale"),
        physical: oxpath!("ui", "locale"),
        access: MountAccess::View,
    }]);
    let ns = oxpath!("components", "panel_a");
    let wrap_id = SubscriptionId("inner@components/panel_a".to_string());
    let wrap = RewritingSubscription::new(
        inner,
        table,
        ns.clone(),
        wrap_id.clone(),
    );

    let mut snap = MapReader::default();
    let change = PathChange {
        path: oxpath!("components", "panel_a", "trigger"),
        before: None,
        after: Some(s("x")),
    };

    struct NoSpawn;
    impl SpawnHandle for NoSpawn {
        fn spawn(&self, _: BoxFuture<()>) -> tokio::task::AbortHandle {
            unreachable!();
        }
    }
    struct NoWriter;
    impl AsyncWriter for NoWriter {
        fn write(&self, _: Path, _: Record) -> BoxFuture<Result<Path, Error>> {
            unreachable!();
        }
    }

    let writer: Arc<dyn AsyncWriter> = Arc::new(NoWriter);
    let ctx = SubCtx {
        snapshot: &mut snap,
        change: &change,
        spawn: &NoSpawn,
        writer,
    };

    let writes = wrap.handle(ctx);

    // Expect a single write: the structured error event at the
    // conventional report path. The offending batch (the inner sub's
    // write to `locale`) is dropped — partial application would
    // silently corrupt component state.
    assert_eq!(writes.len(), 1, "expect one error-event write");
    let expected_path = error_report_path(&ns, &wrap_id);
    assert_eq!(writes[0].path, expected_path);

    let value = writes[0].record.as_value().expect("event has value");
    let event: NamespaceErrorEvent =
        structfs_serde_store::from_value(value.clone()).expect("deserialize");
    assert_eq!(event.kind, NamespaceErrorKind::WriteDenied);
    assert_eq!(event.subscription_id, wrap_id.0);
    assert_eq!(event.path, oxpath!("locale"));
}
```

- [ ] **Step 5.2: Run tests to verify they fail**

Run: `cargo test -p horns-core namespace::tests::rewriting_subscription`
Expected: FAIL with "cannot find type `RewritingSubscription`".

- [ ] **Step 5.3: Implement RewritingSubscription**

Add to `crates/horns-core/src/namespace.rs` above the `#[cfg(test)]` block:

```rust
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::mount::Mount;
use crate::subscription::{PathPattern, SubCtx, Subscription, SubscriptionId};

/// Structured description of a namespace-boundary error. Emitted by
/// `RewritingSubscription` to a well-known broker path so the failure
/// is observable from inside the substrate (tests, debug surfaces,
/// error-aggregating components) and not just from tracing logs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceErrorEvent {
    pub kind: NamespaceErrorKind,
    /// The id of the wrapping subscription that detected the error.
    pub subscription_id: String,
    /// The component-local path the inner subscription tried to act
    /// on. Recorded in component-local terms so the author can map it
    /// back to their source without knowing the wrapper's namespace.
    #[serde(with = "crate::path_serde")]
    pub path: Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamespaceErrorKind {
    /// The inner subscription attempted to write to a
    /// `MountAccess::View` mount (read-only on the component side).
    WriteDenied,
}

/// Conventional broker path for a wrapping subscription's last
/// namespace-boundary error: `<namespace>/_errors/<wrap-id-base>`,
/// where `<wrap-id-base>` is the wrap id with any `@<namespace>`
/// suffix stripped. Subsequent failures overwrite the record — the
/// path holds the *most recent* error, which is what diagnostic
/// surfaces typically want.
pub fn error_report_path(namespace: &Path, wrap_id: &SubscriptionId) -> Path {
    let base = wrap_id.0.split('@').next().unwrap_or(&wrap_id.0);
    let mut components = namespace.components.clone();
    components.push("_errors".to_string());
    components.push(base.to_string());
    Path::try_from_components(components)
        .expect("namespace + _errors + base id is a valid path")
}

/// Wraps a child `Subscription` so that:
/// - its watched patterns are rewritten through a `MountTable` + namespace
///   before being exposed to the broker;
/// - its returned writes are rewritten the same way before being applied.
///
/// The wrapped subscription operates as if its paths were
/// component-local; the broker sees only physical paths.
///
/// **Error path.** If the inner subscription returns a write that
/// violates a mount's access mode (e.g. a write to a `View`-mode
/// mount), the wrapper:
/// 1. Logs the violation at `tracing::error!` level.
/// 2. Drops the entire offending batch — partial application would
///    silently corrupt component state in subtler ways.
/// 3. Emits a single `NamespaceErrorEvent` to `error_report_path` so
///    the failure is observable from broker state. Tests can read
///    the path; debug surfaces can render it; error-aggregating
///    components can subscribe to it.
pub struct RewritingSubscription {
    id: SubscriptionId,
    inner: Arc<dyn Subscription>,
    table: MountTable,
    namespace: Path,
    rewritten_watches: Vec<PathPattern>,
}

impl RewritingSubscription {
    pub fn new(
        inner: Arc<dyn Subscription>,
        table: MountTable,
        namespace: Path,
        id: SubscriptionId,
    ) -> Self {
        let rewritten_watches = inner
            .watches()
            .iter()
            .map(|p| rewrite_pattern(p, &table, &namespace))
            .collect();
        Self {
            id,
            inner,
            table,
            namespace,
            rewritten_watches,
        }
    }
}

impl Subscription for RewritingSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }
    fn watches(&self) -> &[PathPattern] {
        &self.rewritten_watches
    }
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        let inner_writes = self.inner.handle(ctx);
        match rewrite_writes(inner_writes, &self.table, &self.namespace) {
            Ok(w) => w,
            Err(NamespaceError::WriteDenied(local_path)) => {
                tracing::error!(
                    sub = %self.id.0,
                    path = %local_path,
                    "RewritingSubscription dropped a batch: write to read-only mount"
                );
                let event = NamespaceErrorEvent {
                    kind: NamespaceErrorKind::WriteDenied,
                    subscription_id: self.id.0.clone(),
                    path: local_path,
                };
                let value = structfs_serde_store::to_value(&event)
                    .expect("NamespaceErrorEvent is serde-serializable");
                vec![Write {
                    path: error_report_path(&self.namespace, &self.id),
                    record: Record::parsed(value),
                }]
            }
            Err(NamespaceError::ReadDenied(_)) => {
                // `rewrite_writes` only checks write-side access (View
                // mounts), so ReadDenied should never be returned from
                // it. This arm is defensive — keeps the match
                // exhaustive against future NamespaceError variants
                // and surfaces a loud log if the assumption ever
                // changes.
                tracing::error!(
                    sub = %self.id.0,
                    "RewritingSubscription saw unexpected ReadDenied from rewrite_writes"
                );
                Vec::new()
            }
        }
    }
}

/// Rewrite a `PathPattern` through the mount table + namespace.
fn rewrite_pattern(p: &PathPattern, table: &MountTable, namespace: &Path) -> PathPattern {
    match p {
        PathPattern::Exact(p) => PathPattern::Exact(rewrite_path(p, table, namespace)),
        PathPattern::Prefix(p) => PathPattern::Prefix(rewrite_path(p, table, namespace)),
        PathPattern::PrefixSuffix { prefix, suffix } => PathPattern::PrefixSuffix {
            prefix: rewrite_path(prefix, table, namespace),
            suffix: suffix.clone(),
        },
    }
}
```

- [ ] **Step 5.4: Run tests to verify they pass**

Run: `cargo test -p horns-core namespace`
Expected: 8 passing.

- [ ] **Step 5.5: Re-export RewritingSubscription and the error-event types**

Update lib.rs:

```rust
pub use namespace::{
    NamespaceError, NamespaceErrorEvent, NamespaceErrorKind, NamespaceView,
    RewritingSubscription, error_report_path, rewrite_writes,
};
```

Build to verify: `cargo build -p horns-core`.

- [ ] **Step 5.6: Commit**

```bash
git add crates/horns-core/src/namespace.rs crates/horns-core/src/lib.rs
git commit -m "feat(horns-core): add RewritingSubscription wrapping component subscriptions through mount table"
```

---

### Task 6: ComponentInstall API, install/uninstall lifecycle, recursive nesting

**Files:**
- Create: `crates/horns-core/src/component.rs`
- Modify: `crates/horns-core/src/lib.rs`

- [ ] **Step 6.1: Write the module scaffold + author-facing docs + first failing test**

Create `crates/horns-core/src/component.rs`:

```rust
//! # Writing a horns component
//!
//! A horns component is a pure function from an `InstallerScope` to an
//! `InstallBundle`. The bundle declares the subscriptions and metadata
//! writes the component contributes when installed.
//!
//! ```ignore
//! pub fn install(scope: &InstallerScope) -> InstallBundle {
//!     InstallBundle {
//!         subscriptions: vec![/* component's own subs */],
//!         metadata_writes: vec![/* component's seeded paths */],
//!     }
//! }
//! ```
//!
//! The shell installs the component by wrapping that function in a
//! `ComponentInstall` and passing it to `install_component`:
//!
//! ```ignore
//! let handle = install_component(
//!     ComponentInstall {
//!         namespace: oxpath!("components", "panel_a"),
//!         mounts: MountTable::from(vec![Mount {
//!             local: oxpath!("theme"),
//!             physical: oxpath!("ui", "theme"),
//!             access: MountAccess::Shared,
//!         }]),
//!         install_fn: Arc::new(my_component::install),
//!         reactivity: ReactivityMode::Auto,
//!     },
//!     &parent_scope,
//! )?;
//! ```
//!
//! The returned `InstallHandle` carries the subscription ids the
//! framework registered; pass it to `uninstall_component` to tear the
//! component down.
//!
//! ## Recursive nesting
//!
//! A component's `install_fn` receives its own `InstallerScope` and may
//! call `install_component` for its own child components, passing that
//! scope as the parent. Child mount physical paths are resolved through
//! the parent scope, so a child can reference its parent's locals or
//! mounted aliases without knowing the absolute broker path.
//!
//! ## Reactivity
//!
//! By default (`ReactivityMode::Auto`), `install_component` wires a
//! `ComponentReactivitySubscription` that watches the component's
//! namespace prefix AND each resolved mount's physical path. Any change
//! bumps the component's `<namespace>/render_tick`, which the
//! component's `RenderSubscription` (if it has one) listens on. The
//! subscription includes a self-write guard so its own tick writes don't
//! re-trigger it.
//!
//! Opt out with `ReactivityMode::Manual` if the component manages its
//! own re-render triggering.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Record, Value};

use crate::install::InstallBundle;
use crate::mount::{
    InstallerScope, Mount, MountError, MountTable, resolve_child_mount,
};
use crate::namespace::RewritingSubscription;
use crate::path_serde;
use crate::subscription::{PathPattern, SubCtx, Subscription, SubscriptionId};
use crate::write::Write;

/// The component-side install function. Given the scope being installed
/// into, returns the component's own `InstallBundle` populated with
/// subscriptions and metadata writes addressed in **component-local**
/// terms (paths relative to the namespace, or matching mount entries).
pub type ComponentInstallFn = dyn Fn(&InstallerScope) -> InstallBundle + Send + Sync;

/// Reactivity wiring choice for an install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReactivityMode {
    /// Framework auto-wires a `ComponentReactivitySubscription` over
    /// the component's namespace + resolved mount physical paths.
    Auto,
    /// Component manages its own re-render triggering. No reactivity
    /// subscription is added by the framework.
    Manual,
}

/// What the shell needs to install a component.
pub struct ComponentInstall {
    /// Namespace prefix the component's private state lives under,
    /// expressed *relative to the parent scope*.
    pub namespace: Path,
    /// Mount declarations binding the component's local paths to paths
    /// in the parent scope (which the parent scope will further resolve
    /// to broker paths).
    pub mounts: MountTable,
    /// Function the shell calls to obtain the component's own install
    /// bundle.
    pub install_fn: Arc<ComponentInstallFn>,
    /// Whether the framework should auto-wire a reactivity subscription.
    pub reactivity: ReactivityMode,
}

/// Handle returned to the caller of `install_component`. Carries every
/// subscription id the install registered so `uninstall_component` can
/// remove them cleanly.
#[derive(Debug, Clone)]
pub struct InstallHandle {
    /// Subscription ids the install added to the bundle.
    pub subscription_ids: Vec<SubscriptionId>,
    /// The scope this install produced — pass it as the parent scope
    /// when installing child components inside this one.
    pub scope: InstallerScope,
}

/// Errors returned by `install_component`.
#[derive(Debug)]
pub enum InstallComponentError {
    /// The mount table failed validation.
    InvalidMounts(MountError),
}

#[cfg(test)]
mod tests {
    // Tests added incrementally in subsequent steps.
}
```

Add the module to `crates/horns-core/src/lib.rs`:

```rust
pub mod component;
pub use component::{
    ComponentInstall, ComponentInstallFn, InstallComponentError, InstallHandle,
    ReactivityMode,
};
```

Run: `cargo build -p horns-core` to confirm the scaffold compiles.
Expected: builds clean.

- [ ] **Step 6.2: Write failing test for install_component basic behavior**

Append to the `tests` module in `crates/horns-core/src/component.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::InstallBundle;
    use crate::mount::{Mount, MountAccess};
    use crate::subscription::{PathPattern, SubCtx, Subscription, SubscriptionId};
    use ox_path::oxpath;
    use std::sync::Arc;
    use structfs_core_store::{Record, Value};

    struct DummySub {
        id: SubscriptionId,
        watches: Vec<PathPattern>,
    }
    impl Subscription for DummySub {
        fn id(&self) -> &SubscriptionId { &self.id }
        fn watches(&self) -> &[PathPattern] { &self.watches }
        fn handle(&self, _: SubCtx<'_>) -> Vec<Write> { Vec::new() }
    }

    fn empty_install_fn() -> Arc<ComponentInstallFn> {
        Arc::new(|_scope: &InstallerScope| InstallBundle {
            subscriptions: Vec::new(),
            metadata_writes: Vec::new(),
        })
    }

    #[test]
    fn install_component_wraps_subscription_patterns_through_namespace() {
        let install_fn: Arc<ComponentInstallFn> = Arc::new(|_scope| InstallBundle {
            subscriptions: vec![Arc::new(DummySub {
                id: SubscriptionId("inner".to_string()),
                watches: vec![PathPattern::Exact(oxpath!("buffer"))],
            })],
            metadata_writes: Vec::new(),
        });

        let spec = ComponentInstall {
            namespace: oxpath!("components", "panel_a"),
            mounts: MountTable::default(),
            install_fn,
            reactivity: ReactivityMode::Manual,
        };
        let (bundle, _handle) =
            install_component(spec, &InstallerScope::root()).expect("install");

        // Inner subscription's watch is now rewritten to the broker path.
        let inner = bundle.subscriptions.iter()
            .find(|s| s.id().0.starts_with("inner@"))
            .expect("wrapped inner subscription is present");
        assert_eq!(
            inner.watches(),
            &[PathPattern::Exact(oxpath!("components", "panel_a", "buffer"))]
        );
    }

    #[test]
    fn install_component_uses_mount_physical_for_subscription_watches() {
        let install_fn: Arc<ComponentInstallFn> = Arc::new(|_scope| InstallBundle {
            subscriptions: vec![Arc::new(DummySub {
                id: SubscriptionId("watch_theme".to_string()),
                watches: vec![PathPattern::Exact(oxpath!("theme"))],
            })],
            metadata_writes: Vec::new(),
        });

        let spec = ComponentInstall {
            namespace: oxpath!("components", "panel_a"),
            mounts: MountTable::from(vec![Mount {
                local: oxpath!("theme"),
                physical: oxpath!("ui", "theme"),
                access: MountAccess::Shared,
            }]),
            install_fn,
            reactivity: ReactivityMode::Manual,
        };
        let (bundle, _handle) =
            install_component(spec, &InstallerScope::root()).expect("install");

        let inner = bundle.subscriptions.iter()
            .find(|s| s.id().0.starts_with("watch_theme@"))
            .expect("watch_theme wrapped");
        assert_eq!(
            inner.watches(),
            &[PathPattern::Exact(oxpath!("ui", "theme"))]
        );
    }

    #[test]
    fn install_handle_includes_subscription_ids_for_uninstall() {
        let install_fn: Arc<ComponentInstallFn> = Arc::new(|_scope| InstallBundle {
            subscriptions: vec![Arc::new(DummySub {
                id: SubscriptionId("inner".to_string()),
                watches: Vec::new(),
            })],
            metadata_writes: Vec::new(),
        });

        let spec = ComponentInstall {
            namespace: oxpath!("c", "x"),
            mounts: MountTable::default(),
            install_fn,
            reactivity: ReactivityMode::Manual,
        };
        let (_bundle, handle) =
            install_component(spec, &InstallerScope::root()).expect("install");

        // Manual reactivity → only the inner wrapped subscription is present.
        assert_eq!(handle.subscription_ids.len(), 1);
        assert!(handle.subscription_ids[0].0.starts_with("inner@"));
    }

    #[test]
    fn install_component_returns_error_on_duplicate_mount_locals() {
        let spec = ComponentInstall {
            namespace: oxpath!("c", "x"),
            mounts: MountTable::from(vec![
                Mount {
                    local: oxpath!("dup"),
                    physical: oxpath!("a"),
                    access: MountAccess::Shared,
                },
                Mount {
                    local: oxpath!("dup"),
                    physical: oxpath!("b"),
                    access: MountAccess::Shared,
                },
            ]),
            install_fn: empty_install_fn(),
            reactivity: ReactivityMode::Manual,
        };
        let err = install_component(spec, &InstallerScope::root()).unwrap_err();
        assert!(matches!(err, InstallComponentError::InvalidMounts(_)));
    }
}
```

- [ ] **Step 6.3: Run tests to verify they fail**

Run: `cargo test -p horns-core component`
Expected: FAIL with "cannot find function `install_component`".

- [ ] **Step 6.4: Implement install_component and uninstall_component**

Add to `crates/horns-core/src/component.rs` above the `#[cfg(test)]` block:

```rust
/// Install a component at the given scope. The component's mount
/// physical paths are resolved through `parent_scope`, producing the
/// effective mount table for this component. The component's
/// `install_fn` is invoked with its own (child) scope so it can install
/// further nested components.
///
/// Returns the `InstallBundle` (subscriptions + metadata writes) and an
/// `InstallHandle` carrying the subscription ids and the child scope.
pub fn install_component(
    spec: ComponentInstall,
    parent_scope: &InstallerScope,
) -> Result<(InstallBundle, InstallHandle), InstallComponentError> {
    spec.mounts.validate().map_err(InstallComponentError::InvalidMounts)?;

    // Resolve child mount physical paths through the parent scope.
    let effective_mounts = MountTable::from(
        spec.mounts
            .entries()
            .iter()
            .map(|m| resolve_child_mount(m, parent_scope))
            .collect::<Vec<_>>(),
    );

    // Build this component's effective namespace and scope.
    let effective_namespace = parent_scope.resolve(&spec.namespace);
    let this_scope = InstallerScope::new(
        effective_namespace.clone(),
        effective_mounts.clone(),
    );

    // Invoke the component to obtain its inner bundle.
    let inner = (spec.install_fn)(&this_scope);

    // Wrap each inner subscription with RewritingSubscription. Track
    // ids for uninstall.
    let mut subscription_ids: Vec<SubscriptionId> = Vec::new();
    let mut subscriptions: Vec<Arc<dyn Subscription>> = Vec::new();
    for sub in inner.subscriptions {
        let wrap_id = SubscriptionId(format!(
            "{}@{}",
            sub.id().0,
            effective_namespace.to_string()
        ));
        subscription_ids.push(wrap_id.clone());
        subscriptions.push(Arc::new(RewritingSubscription::new(
            sub,
            effective_mounts.clone(),
            effective_namespace.clone(),
            wrap_id,
        )) as Arc<dyn Subscription>);
    }

    // Rewrite metadata write paths through the same effective mounts +
    // namespace. Use `rewrite_path` from `mount.rs` so this stays in
    // sync with the Reader rewriter.
    let mut metadata_writes: Vec<(Path, Record)> = inner
        .metadata_writes
        .into_iter()
        .map(|(p, r)| {
            (
                crate::mount::rewrite_path(&p, &effective_mounts, &effective_namespace),
                r,
            )
        })
        .collect();

    // Mount metadata rows: one per entry, keyed by local-path's last
    // component for direct lookup. (See Task 9 for the writes added
    // here; this comment is placeholder until that task lands.)
    let _ = &mut metadata_writes; // suppress unused-mut until Task 9

    let bundle = InstallBundle {
        subscriptions,
        metadata_writes,
    };
    let handle = InstallHandle {
        subscription_ids,
        scope: this_scope,
    };
    Ok((bundle, handle))
}

/// Tear down a component install by unregistering every subscription
/// in the handle from the given registry.
///
/// Callers are responsible for additionally clearing any persistent
/// store state the component wrote (typically a cascade-delete of
/// `<namespace>/...`) — the framework does not do this automatically
/// because the right teardown semantics vary (some tests want to
/// inspect the post-uninstall state).
pub fn uninstall_component(
    handle: &InstallHandle,
    registry: &mut crate::subscription::SubscriptionRegistry,
) -> usize {
    let mut removed = 0;
    for id in &handle.subscription_ids {
        removed += registry.unregister(id);
    }
    removed
}
```

- [ ] **Step 6.5: Run tests to verify they pass**

Run: `cargo test -p horns-core component`
Expected: 4 passing.

- [ ] **Step 6.6: Add failing test for recursive nesting (mount resolution through parent scope)**

Append to the `tests` module:

```rust
#[test]
fn child_install_resolves_mount_physical_through_parent_scope() {
    // Parent scope: namespace `app/screens`, with `inherited_theme` →
    // `ui/theme` mount.
    let parent_scope = InstallerScope::new(
        oxpath!("app", "screens"),
        MountTable::from(vec![Mount {
            local: oxpath!("inherited_theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        }]),
    );

    // Child component declares its mount referencing
    // `inherited_theme` in the parent scope's local terms.
    let install_fn: Arc<ComponentInstallFn> = Arc::new(|_scope| InstallBundle {
        subscriptions: vec![Arc::new(DummySub {
            id: SubscriptionId("child_sub".to_string()),
            watches: vec![PathPattern::Exact(oxpath!("theme"))],
        })],
        metadata_writes: Vec::new(),
    });
    let spec = ComponentInstall {
        namespace: oxpath!("child"),
        mounts: MountTable::from(vec![Mount {
            local: oxpath!("theme"),
            physical: oxpath!("inherited_theme"),
            access: MountAccess::Shared,
        }]),
        install_fn,
        reactivity: ReactivityMode::Manual,
    };

    let (bundle, handle) =
        install_component(spec, &parent_scope).expect("install");

    // Child's effective scope namespace is parent.resolve(child.namespace).
    // Since `child` isn't in parent's mounts, it gets the parent
    // namespace prefix → `app/screens/child`.
    assert_eq!(handle.scope.namespace(), &oxpath!("app", "screens", "child"));

    // Child's wrapped sub watches the FULLY RESOLVED physical path
    // (`ui/theme`), not the parent's local `inherited_theme`.
    let child_sub = bundle.subscriptions.iter()
        .find(|s| s.id().0.starts_with("child_sub@"))
        .expect("child sub wrapped");
    assert_eq!(
        child_sub.watches(),
        &[PathPattern::Exact(oxpath!("ui", "theme"))]
    );
}
```

- [ ] **Step 6.7: Run tests to verify the new one passes**

Run: `cargo test -p horns-core component::tests::child_install_resolves_mount_physical_through_parent_scope`
Expected: PASS (the implementation in 6.4 already handles this — `resolve_child_mount` runs the child's physical through `parent_scope`, and `effective_namespace` uses `parent_scope.resolve` for the namespace.).

If it fails, double-check that `install_component` in step 6.4 calls `parent_scope.resolve(&spec.namespace)` for the effective namespace and `resolve_child_mount(m, parent_scope)` for each entry.

Run the whole component test set: `cargo test -p horns-core component`
Expected: 5 passing.

- [ ] **Step 6.8: Re-export install/uninstall_component**

Update `crates/horns-core/src/lib.rs`:

```rust
pub use component::{
    ComponentInstall, ComponentInstallFn, InstallComponentError, InstallHandle,
    ReactivityMode, install_component, uninstall_component,
};
```

Run: `cargo build -p horns-core`.

- [ ] **Step 6.9: Commit**

```bash
git add crates/horns-core/src/component.rs crates/horns-core/src/lib.rs
git commit -m "feat(horns-core): install_component / uninstall_component with InstallHandle and parent-scope mount resolution"
```

---

### Task 7: ComponentReactivitySubscription with self-write guard

**Files:**
- Modify: `crates/horns-core/src/component.rs`

- [ ] **Step 7.1: Write failing tests**

Append to the `tests` module in `crates/horns-core/src/component.rs`:

```rust
#[test]
fn reactivity_subscription_bumps_tick_on_namespace_write() {
    use crate::subscription::PathChange;

    let sub = ComponentReactivitySubscription::new(
        SubscriptionId("reactivity@app/screens/panel_a".to_string()),
        oxpath!("app", "screens", "panel_a"),
        MountTable::default(),
        oxpath!("app", "screens", "panel_a", "render_tick"),
    );

    let change = PathChange {
        path: oxpath!("app", "screens", "panel_a", "buffer"),
        before: None,
        after: Some(Record::parsed(Value::String("x".into()))),
    };

    let writes = run_handler(&sub, &change);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("app", "screens", "panel_a", "render_tick"));
    match &writes[0].record {
        Record::Parsed(Value::Integer(n)) => assert_eq!(*n, 1),
        other => panic!("expected Integer(1), got {other:?}"),
    }
}

#[test]
fn reactivity_subscription_bumps_tick_on_mounted_physical_write() {
    use crate::subscription::PathChange;

    let mounts = MountTable::from(vec![Mount {
        local: oxpath!("theme"),
        physical: oxpath!("ui", "theme"),
        access: MountAccess::Shared,
    }]);
    let sub = ComponentReactivitySubscription::new(
        SubscriptionId("reactivity@components/panel_a".to_string()),
        oxpath!("components", "panel_a"),
        mounts,
        oxpath!("components", "panel_a", "render_tick"),
    );

    let change = PathChange {
        path: oxpath!("ui", "theme"),
        before: None,
        after: Some(Record::parsed(Value::String("dark".into()))),
    };

    let writes = run_handler(&sub, &change);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("components", "panel_a", "render_tick"));
}

#[test]
fn reactivity_subscription_self_write_guard_breaks_loop() {
    use crate::subscription::PathChange;

    let sub = ComponentReactivitySubscription::new(
        SubscriptionId("reactivity@c/p".to_string()),
        oxpath!("c", "p"),
        MountTable::default(),
        oxpath!("c", "p", "render_tick"),
    );

    // A write to the render-tick path itself must NOT bump (else infinite
    // loop bounded only by cascade-bound).
    let change = PathChange {
        path: oxpath!("c", "p", "render_tick"),
        before: Some(Record::parsed(Value::Integer(0))),
        after: Some(Record::parsed(Value::Integer(1))),
    };

    let writes = run_handler(&sub, &change);
    assert!(writes.is_empty(), "self-writes to render_tick must not bump");
}

#[test]
fn reactivity_subscription_watches_prefix_and_mount_physicals() {
    let mounts = MountTable::from(vec![
        Mount {
            local: oxpath!("theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        },
        Mount {
            local: oxpath!("users"),
            physical: oxpath!("data", "users"),
            access: MountAccess::Shared,
        },
    ]);
    let sub = ComponentReactivitySubscription::new(
        SubscriptionId("reactivity@c/p".to_string()),
        oxpath!("c", "p"),
        mounts,
        oxpath!("c", "p", "render_tick"),
    );

    let watches = sub.watches();
    assert!(watches.contains(&PathPattern::Prefix(oxpath!("c", "p"))));
    assert!(watches.contains(&PathPattern::Exact(oxpath!("ui", "theme"))));
    assert!(watches.contains(&PathPattern::Exact(oxpath!("data", "users"))));
}

// Helper that drives a subscription's handle() with a minimal SubCtx
// pointing at an EmptyReader.
fn run_handler(sub: &dyn Subscription, change: &crate::subscription::PathChange) -> Vec<Write> {
    use crate::subscription::{AsyncWriter, BoxFuture, SpawnHandle, SubCtx};
    use structfs_core_store::{Error as StoreError, Reader};

    struct EmptyReader;
    impl Reader for EmptyReader {
        fn read(&mut self, _: &Path) -> Result<Option<Record>, StoreError> { Ok(None) }
    }
    struct NoSpawn;
    impl SpawnHandle for NoSpawn {
        fn spawn(&self, _: BoxFuture<()>) -> tokio::task::AbortHandle {
            unreachable!("test does not exercise spawn");
        }
    }
    struct NoWriter;
    impl AsyncWriter for NoWriter {
        fn write(&self, _: Path, _: Record) -> BoxFuture<Result<Path, StoreError>> {
            unreachable!("test does not exercise async writer");
        }
    }

    let mut snap = EmptyReader;
    let writer: Arc<dyn AsyncWriter> = Arc::new(NoWriter);
    let ctx = SubCtx {
        snapshot: &mut snap,
        change,
        spawn: &NoSpawn,
        writer,
    };
    sub.handle(ctx)
}
```

- [ ] **Step 7.2: Run tests to verify they fail**

Run: `cargo test -p horns-core component::tests::reactivity_subscription`
Expected: FAIL with "cannot find type `ComponentReactivitySubscription`".

- [ ] **Step 7.3: Implement ComponentReactivitySubscription**

Add to `crates/horns-core/src/component.rs` above the `#[cfg(test)]` block:

```rust
/// Coarse per-component re-render trigger. Watches the component's
/// namespace prefix AND each resolved mount's physical path; on any
/// change, emits a single write that bumps the component's render-tick
/// path.
///
/// Self-write guard: a change at the render_tick_path itself is
/// ignored, breaking the obvious infinite cascade. (Cascade-bound
/// would catch a runaway anyway, but the guard makes the model honest.)
pub struct ComponentReactivitySubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    render_tick_path: Path,
}

impl ComponentReactivitySubscription {
    pub fn new(
        id: SubscriptionId,
        namespace: Path,
        mounts: MountTable,
        render_tick_path: Path,
    ) -> Self {
        let mut watches: Vec<PathPattern> = Vec::with_capacity(1 + mounts.entries().len());
        watches.push(PathPattern::Prefix(namespace));
        for m in mounts.entries() {
            watches.push(PathPattern::Exact(m.physical.clone()));
        }
        Self { id, watches, render_tick_path }
    }
}

impl Subscription for ComponentReactivitySubscription {
    fn id(&self) -> &SubscriptionId { &self.id }
    fn watches(&self) -> &[PathPattern] { &self.watches }
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        if ctx.change.path.components == self.render_tick_path.components {
            return Vec::new();
        }
        let current = ctx
            .snapshot
            .read(&self.render_tick_path)
            .ok()
            .flatten()
            .and_then(|r| match r.as_value() {
                Some(Value::Integer(n)) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        vec![Write {
            path: self.render_tick_path.clone(),
            record: Record::parsed(Value::Integer(current.wrapping_add(1))),
        }]
    }
}
```

- [ ] **Step 7.4: Run tests to verify they pass**

Run: `cargo test -p horns-core component`
Expected: 9 passing.

- [ ] **Step 7.5: Re-export from lib.rs**

Update `crates/horns-core/src/lib.rs`:

```rust
pub use component::{
    ComponentInstall, ComponentInstallFn, ComponentReactivitySubscription,
    InstallComponentError, InstallHandle, ReactivityMode, install_component,
    uninstall_component,
};
```

Build: `cargo build -p horns-core`.

- [ ] **Step 7.6: Commit**

```bash
git add crates/horns-core/src/component.rs crates/horns-core/src/lib.rs
git commit -m "feat(horns-core): add ComponentReactivitySubscription with self-write guard"
```

---

### Task 8: Auto-wire reactivity in install_component

**Files:**
- Modify: `crates/horns-core/src/component.rs`

- [ ] **Step 8.1: Write failing test**

Append to the `tests` module in `crates/horns-core/src/component.rs`:

```rust
#[test]
fn install_component_auto_wires_reactivity_subscription() {
    let spec = ComponentInstall {
        namespace: oxpath!("components", "panel_a"),
        mounts: MountTable::from(vec![Mount {
            local: oxpath!("theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        }]),
        install_fn: empty_install_fn(),
        reactivity: ReactivityMode::Auto,
    };
    let (bundle, handle) =
        install_component(spec, &InstallerScope::root()).expect("install");

    // A reactivity subscription was added.
    let reactivity = bundle.subscriptions.iter()
        .find(|s| s.id().0.starts_with("reactivity@"))
        .expect("auto-wired reactivity subscription is present");

    // Its watches include the namespace prefix and the mounted physical.
    let watches = reactivity.watches();
    assert!(watches.contains(&PathPattern::Prefix(oxpath!("components", "panel_a"))));
    assert!(watches.contains(&PathPattern::Exact(oxpath!("ui", "theme"))));

    // Handle subscription_ids includes the reactivity id.
    assert!(handle.subscription_ids.iter().any(|id| id.0.starts_with("reactivity@")));
}

#[test]
fn install_component_skips_reactivity_when_manual() {
    let spec = ComponentInstall {
        namespace: oxpath!("components", "panel_a"),
        mounts: MountTable::default(),
        install_fn: empty_install_fn(),
        reactivity: ReactivityMode::Manual,
    };
    let (bundle, _handle) =
        install_component(spec, &InstallerScope::root()).expect("install");

    // No reactivity subscription added.
    assert!(
        !bundle.subscriptions.iter().any(|s| s.id().0.starts_with("reactivity@")),
        "Manual mode must not auto-wire reactivity"
    );
}
```

- [ ] **Step 8.2: Run tests to verify they fail**

Run: `cargo test -p horns-core component::tests::install_component_auto_wires_reactivity_subscription`
Expected: FAIL — no reactivity subscription added.

- [ ] **Step 8.3: Modify install_component to auto-wire reactivity**

In `crates/horns-core/src/component.rs`, modify `install_component`. After the existing `subscriptions` Vec is populated and before constructing the `InstallBundle`, add the auto-wired reactivity subscription:

```rust
    // (existing) wrap inner subscriptions...
    // (existing) rewrite metadata writes...

    // Auto-wire reactivity if requested.
    if matches!(spec.reactivity, ReactivityMode::Auto) {
        let mut tick_components = effective_namespace.components.clone();
        tick_components.push("render_tick".to_string());
        let render_tick_path = Path::try_from_components(tick_components)
            .expect("namespace + render_tick is a valid path");

        let reactivity_id = SubscriptionId(format!(
            "reactivity@{}",
            effective_namespace.to_string()
        ));
        subscription_ids.push(reactivity_id.clone());
        subscriptions.push(Arc::new(ComponentReactivitySubscription::new(
            reactivity_id,
            effective_namespace.clone(),
            effective_mounts.clone(),
            render_tick_path,
        )) as Arc<dyn Subscription>);
    }

    let bundle = InstallBundle {
        subscriptions,
        metadata_writes,
    };
    // (existing) construct handle and return
```

Make sure this block lands *before* the `let bundle = ...` line and `subscriptions` is still mutable at that point.

- [ ] **Step 8.4: Run tests to verify they pass**

Run: `cargo test -p horns-core component`
Expected: 11 passing.

- [ ] **Step 8.5: Commit**

```bash
git add crates/horns-core/src/component.rs
git commit -m "feat(horns-core): auto-wire ComponentReactivitySubscription in install_component (with Manual opt-out)"
```

---

### Task 9: Mount metadata writes keyed by local path

**Files:**
- Modify: `crates/horns-core/src/component.rs`

- [ ] **Step 9.1: Write failing test**

Append to the `tests` module:

```rust
#[test]
fn install_component_emits_mount_metadata_writes_keyed_by_local() {
    let spec = ComponentInstall {
        namespace: oxpath!("components", "panel_a"),
        mounts: MountTable::from(vec![
            Mount {
                local: oxpath!("theme"),
                physical: oxpath!("ui", "theme"),
                access: MountAccess::Shared,
            },
            Mount {
                local: oxpath!("output"),
                physical: oxpath!("ui", "panel_a_out"),
                access: MountAccess::Output,
            },
        ]),
        install_fn: empty_install_fn(),
        reactivity: ReactivityMode::Manual,
    };
    let (bundle, _handle) =
        install_component(spec, &InstallerScope::root()).expect("install");

    // Expect one metadata row per mount, keyed by the local path's
    // last segment (so single-component locals are directly addressable
    // under <namespace>/mounts/<segment>).
    let theme_meta = bundle.metadata_writes.iter()
        .find(|(p, _)| p == &oxpath!("components", "panel_a", "mounts", "theme"));
    let output_meta = bundle.metadata_writes.iter()
        .find(|(p, _)| p == &oxpath!("components", "panel_a", "mounts", "output"));
    assert!(theme_meta.is_some(), "expected mount metadata row for `theme`");
    assert!(output_meta.is_some(), "expected mount metadata row for `output`");

    // Serialized Mount roundtrips.
    if let Some((_, Record::Parsed(v))) = theme_meta {
        let m: Mount = structfs_serde_store::from_value(v.clone()).expect("deserialize");
        assert_eq!(m.local, oxpath!("theme"));
        assert_eq!(m.physical, oxpath!("ui", "theme"));
        assert_eq!(m.access, MountAccess::Shared);
    } else {
        panic!("expected Parsed record for theme mount");
    }
}
```

- [ ] **Step 9.2: Run test to verify it fails**

Run: `cargo test -p horns-core component::tests::install_component_emits_mount_metadata_writes_keyed_by_local`
Expected: FAIL — no mount metadata rows present.

- [ ] **Step 9.3: Implement mount metadata writes**

In `crates/horns-core/src/component.rs`, modify `install_component`. After the existing `metadata_writes` is constructed (and after the auto-reactivity block from Task 8), add mount-metadata writes:

```rust
    // Mount metadata: one row per Mount entry, keyed under
    // <effective_namespace>/mounts/<local-last-segment>. Single-component
    // locals are directly addressable; multi-component locals fall back
    // to a joined slug. Keeps introspection lookups O(1) for the
    // common case.
    for mount in spec.mounts.entries() {
        let key_segment: String = if mount.local.components.len() == 1 {
            mount.local.components[0].clone()
        } else {
            mount.local.components.join("__")
        };
        let mut components = effective_namespace.components.clone();
        components.push("mounts".to_string());
        components.push(key_segment);
        let path = Path::try_from_components(components)
            .expect("namespace + mounts + segment is a valid path");
        // Note: serialize the ORIGINAL (unresolved) Mount as authored —
        // introspection consumers care about the declared shape, not
        // the resolved broker paths. The resolved broker paths can be
        // recomputed from parent-scope metadata if needed.
        let value = structfs_serde_store::to_value(mount)
            .expect("Mount is serde-serializable");
        metadata_writes.push((path, Record::parsed(value)));
    }
```

Place this block between the existing inner-bundle metadata rewriting and the auto-reactivity block — order doesn't affect correctness but keeping introspection writes before reactivity wiring keeps the writes grouped by intent.

- [ ] **Step 9.4: Run tests to verify they pass**

Run: `cargo test -p horns-core component`
Expected: 12 passing.

- [ ] **Step 9.5: Commit**

```bash
git add crates/horns-core/src/component.rs
git commit -m "feat(horns-core): emit mount metadata writes keyed by local path segment for introspection"
```

---

### Task 10: End-to-end test — one-level install with real RewritingSubscription

**Files:**
- Create: `crates/horns-core/tests/component_nesting_e2e.rs`

- [ ] **Step 10.1: Write the one-level e2e test**

Create `crates/horns-core/tests/component_nesting_e2e.rs`:

```rust
//! End-to-end tests for component nesting through a real BrokerStore.
//!
//! Two scenarios:
//! 1. `one_level_install_with_rewriting_subscription_writes_back_through_mount`
//!    — exercises `RewritingSubscription` end-to-end. A component
//!    subscribes to a private path; on fire, returns a write to its
//!    output mount; the shell sees the write at the physical path.
//! 2. `two_level_nested_install_shares_mount_across_depth` — exercises
//!    recursive nesting. A sub-shell installs a panel that mounts
//!    `theme` from the sub-shell's `inherited_theme`, which the
//!    outer shell mounted from `ui/theme`. A write to `ui/theme`
//!    triggers the panel's reactivity through both layers.
//!
//! Harness pattern follows `install_e2e.rs`: real BrokerStore over an
//! in-test MemoryStore, multi-thread tokio.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use horns_core::install::InstallBundle;
use horns_core::mount::Mount;
use horns_core::{
    ComponentInstall, ComponentInstallFn, InstallerScope, MountAccess, MountTable,
    PathPattern, ReactivityMode, SubCtx, Subscription, SubscriptionId, Write,
    install_component,
};
use ox_broker::BrokerStore;
use ox_path::oxpath;
use parking_lot::Mutex;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

struct MemoryStore {
    data: BTreeMap<String, Value>,
}

impl MemoryStore {
    fn new() -> Self {
        Self { data: BTreeMap::new() }
    }
}

impl Reader for MemoryStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        Ok(self.data.get(&from.to_string()).map(|v| Record::parsed(v.clone())))
    }
}

impl Writer for MemoryStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if let Some(value) = data.as_value() {
            self.data.insert(to.to_string(), value.clone());
        }
        Ok(to.clone())
    }
}

/// A component that subscribes to a private path and, on fire, writes
/// to its component-local `output` mount. Used to exercise
/// `RewritingSubscription` through a real broker.
struct WriteOnTriggerSub {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
}
impl Subscription for WriteOnTriggerSub {
    fn id(&self) -> &SubscriptionId { &self.id }
    fn watches(&self) -> &[PathPattern] { &self.watches }
    fn handle(&self, _: SubCtx<'_>) -> Vec<Write> {
        vec![Write {
            path: oxpath!("output"),  // component-local; mount rewrites it
            record: Record::parsed(Value::String("from_trigger".into())),
        }]
    }
}

fn write_on_trigger_component() -> Arc<ComponentInstallFn> {
    Arc::new(|_scope: &InstallerScope| InstallBundle {
        subscriptions: vec![Arc::new(WriteOnTriggerSub {
            id: SubscriptionId("write_on_trigger".to_string()),
            watches: vec![PathPattern::Exact(oxpath!("trigger"))],
        })],
        metadata_writes: Vec::new(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_level_install_with_rewriting_subscription_writes_back_through_mount() {
    let store = Arc::new(Mutex::new(MemoryStore::new()));
    let broker = BrokerStore::new(store.clone());
    let client = broker.client();

    let spec = ComponentInstall {
        namespace: oxpath!("components", "writer"),
        mounts: MountTable::from(vec![Mount {
            local: oxpath!("output"),
            physical: oxpath!("ui", "writer_out"),
            access: MountAccess::Output,
        }]),
        install_fn: write_on_trigger_component(),
        reactivity: ReactivityMode::Manual,
    };
    let (bundle, _handle) =
        install_component(spec, &InstallerScope::root()).expect("install");

    for (path, record) in bundle.metadata_writes {
        client.write(&path, record).await.expect("metadata write");
    }
    for sub in bundle.subscriptions {
        broker.register_subscription(sub);
    }

    // Fire the subscription by writing the component's trigger path
    // (rewritten to <namespace>/trigger by the framework — the test
    // writes that physical path directly because we're acting as the
    // broker-facing world).
    client
        .write(
            &oxpath!("components", "writer", "trigger"),
            Record::parsed(Value::String("go".into())),
        )
        .await
        .expect("write trigger");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // The sub returned a write to local `output`; RewritingSubscription
    // mapped it to physical `ui/writer_out`. The store should hold the
    // sentinel string there.
    let result = client
        .read(&oxpath!("ui", "writer_out"))
        .await
        .expect("read writer_out");
    let value = result
        .and_then(|r| match r.as_value() {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        });
    assert_eq!(value.as_deref(), Some("from_trigger"));
}
```

- [ ] **Step 10.2: Run the test**

Run: `cargo test -p horns-core --test component_nesting_e2e one_level`
Expected: PASS.

If harness construction (`BrokerStore::new`, `client()` shape, etc.) drifts from `install_e2e.rs`, align with the latest pattern there. The test logic — write to component's trigger path, observe write at mounted physical — is what we want to preserve.

- [ ] **Step 10.3: Commit**

```bash
git add crates/horns-core/tests/component_nesting_e2e.rs
git commit -m "test(horns-core): e2e RewritingSubscription through broker (write rewrites via output mount)"
```

- [ ] **Step 10.4: Write the error-path e2e test**

Append to `crates/horns-core/tests/component_nesting_e2e.rs`:

```rust
/// A component that, on trigger, writes to a `View` (read-only) mount.
/// Used to exercise the error-event path of `RewritingSubscription`
/// end-to-end through the broker.
struct WriteToViewMountSub {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
}
impl Subscription for WriteToViewMountSub {
    fn id(&self) -> &SubscriptionId { &self.id }
    fn watches(&self) -> &[PathPattern] { &self.watches }
    fn handle(&self, _: SubCtx<'_>) -> Vec<Write> {
        vec![Write {
            path: oxpath!("locale"),  // View mount — write will be denied
            record: Record::parsed(Value::String("en".into())),
        }]
    }
}

fn write_to_view_mount_component() -> Arc<ComponentInstallFn> {
    Arc::new(|_scope: &InstallerScope| InstallBundle {
        subscriptions: vec![Arc::new(WriteToViewMountSub {
            id: SubscriptionId("write_to_view".to_string()),
            watches: vec![PathPattern::Exact(oxpath!("trigger"))],
        })],
        metadata_writes: Vec::new(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_to_view_mount_surfaces_error_event_through_broker() {
    use horns_core::namespace::{NamespaceErrorEvent, NamespaceErrorKind, error_report_path};

    let store = Arc::new(Mutex::new(MemoryStore::new()));
    let broker = BrokerStore::new(store.clone());
    let client = broker.client();

    let spec = ComponentInstall {
        namespace: oxpath!("components", "viewer"),
        mounts: MountTable::from(vec![Mount {
            local: oxpath!("locale"),
            physical: oxpath!("ui", "locale"),
            access: MountAccess::View,  // read-only on the component side
        }]),
        install_fn: write_to_view_mount_component(),
        reactivity: ReactivityMode::Manual,
    };
    let (bundle, _handle) =
        install_component(spec, &InstallerScope::root()).expect("install");

    for (path, record) in bundle.metadata_writes {
        client.write(&path, record).await.expect("metadata write");
    }
    for sub in bundle.subscriptions {
        broker.register_subscription(sub);
    }

    // Fire the inner subscription. It will return a write to the View
    // mount; the wrapper rejects it and emits a NamespaceErrorEvent.
    client
        .write(
            &oxpath!("components", "viewer", "trigger"),
            Record::parsed(Value::String("go".into())),
        )
        .await
        .expect("write trigger");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // The view mount's physical path must NOT have been written.
    let locale = client.read(&oxpath!("ui", "locale")).await.expect("read locale");
    assert!(locale.is_none(), "denied write must not reach the View mount's physical path");

    // The error event must be readable at the conventional report path.
    let wrap_id = SubscriptionId(format!(
        "write_to_view@{}",
        oxpath!("components", "viewer").to_string()
    ));
    let report_path = error_report_path(&oxpath!("components", "viewer"), &wrap_id);
    let record = client
        .read(&report_path)
        .await
        .expect("read error report")
        .expect("error report present");
    let value = record.as_value().expect("event has value");
    let event: NamespaceErrorEvent =
        structfs_serde_store::from_value(value.clone()).expect("deserialize event");
    assert_eq!(event.kind, NamespaceErrorKind::WriteDenied);
    assert_eq!(event.path, oxpath!("locale"));
    assert!(
        event.subscription_id.starts_with("write_to_view@"),
        "subscription id should identify the wrapping sub: got {}",
        event.subscription_id,
    );
}
```

- [ ] **Step 10.5: Run the error-path test**

Run: `cargo test -p horns-core --test component_nesting_e2e write_to_view_mount`
Expected: PASS.

Run the whole e2e file: `cargo test -p horns-core --test component_nesting_e2e`
Expected: 2 passing.

- [ ] **Step 10.6: Commit**

```bash
git add crates/horns-core/tests/component_nesting_e2e.rs
git commit -m "test(horns-core): e2e namespace error event surfaces at conventional broker path"
```

---

### Task 11: End-to-end test — two-level recursive nesting

**Files:**
- Modify: `crates/horns-core/tests/component_nesting_e2e.rs`

- [ ] **Step 11.1: Write the two-level e2e test**

Append to `crates/horns-core/tests/component_nesting_e2e.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_level_nested_install_shares_mount_across_depth() {
    let store = Arc::new(Mutex::new(MemoryStore::new()));
    let broker = BrokerStore::new(store.clone());
    let client = broker.client();

    // Sub-shell component: when installed, also installs a child panel
    // that mounts `theme` from the sub-shell's `inherited_theme`. The
    // panel uses auto-reactivity, so a write to `ui/theme` bumps its
    // render_tick through both mount levels.
    let sub_shell_install: Arc<ComponentInstallFn> = Arc::new(|scope: &InstallerScope| {
        // Install the panel inside this sub-shell.
        let panel_spec = ComponentInstall {
            namespace: oxpath!("panel"),
            mounts: MountTable::from(vec![Mount {
                local: oxpath!("theme"),
                // References the sub-shell's local — resolves through
                // its scope to the real broker path.
                physical: oxpath!("inherited_theme"),
                access: MountAccess::Shared,
            }]),
            install_fn: Arc::new(|_scope| InstallBundle {
                subscriptions: Vec::new(),
                metadata_writes: vec![(
                    oxpath!("render_tick"),
                    Record::parsed(Value::Integer(0)),
                )],
            }),
            reactivity: ReactivityMode::Auto,
        };
        let (panel_bundle, _panel_handle) =
            install_component(panel_spec, scope).expect("panel install");

        // Merge panel's bundle into the sub-shell's bundle. The
        // sub-shell itself adds no subscriptions of its own.
        InstallBundle {
            subscriptions: panel_bundle.subscriptions,
            metadata_writes: panel_bundle.metadata_writes,
        }
    });

    let sub_shell_spec = ComponentInstall {
        namespace: oxpath!("sub_shell"),
        mounts: MountTable::from(vec![Mount {
            local: oxpath!("inherited_theme"),
            physical: oxpath!("ui", "theme"),
            access: MountAccess::Shared,
        }]),
        install_fn: sub_shell_install,
        reactivity: ReactivityMode::Manual,  // sub-shell itself doesn't re-render
    };

    let (bundle, _handle) =
        install_component(sub_shell_spec, &InstallerScope::root()).expect("sub_shell install");

    for (path, record) in bundle.metadata_writes {
        client.write(&path, record).await.expect("metadata write");
    }
    for sub in bundle.subscriptions {
        broker.register_subscription(sub);
    }

    // Write to the outermost physical path.
    client
        .write(&oxpath!("ui", "theme"), Record::parsed(Value::String("dark".into())))
        .await
        .expect("write theme");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // The panel's render_tick (at sub_shell/panel/render_tick) must
    // have been bumped to 1 — proof that the mount resolved through
    // two levels and reactivity fired.
    let tick = client
        .read(&oxpath!("sub_shell", "panel", "render_tick"))
        .await
        .expect("read panel tick")
        .and_then(|r| match r.as_value() {
            Some(Value::Integer(n)) => Some(*n),
            _ => None,
        });
    assert_eq!(tick, Some(1), "panel render_tick should bump via two-level mount");
}
```

- [ ] **Step 11.2: Run the test**

Run: `cargo test -p horns-core --test component_nesting_e2e two_level`
Expected: PASS.

Run the whole e2e file: `cargo test -p horns-core --test component_nesting_e2e`
Expected: 3 passing (one-level + error-event + two-level).

- [ ] **Step 11.3: Commit**

```bash
git add crates/horns-core/tests/component_nesting_e2e.rs
git commit -m "test(horns-core): e2e two-level nesting with shared mount across depth"
```

---

## What this plan leaves for follow-up plans

Deliberately out of scope here:

- **Fine-grained reactivity wiring.** `TrackedReader` ships in this plan but the framework does not yet consume its `into_deps()` output to register per-dep exact subscriptions. A follow-up can replace the coarse `ComponentReactivitySubscription` with a tracker-driven equivalent if the coarse version proves too expensive.
- **`View::Nested { area, path }` embedding variant.** Composing a child's published View into a shell's View tree by reference is the next half of the Rio analogy and is a separate, self-contained plan.
- **Layering / Overlay renderer trait.** The earlier conversation around modal/overlay layering is its own framework extension.
- **Reconciler + `View::Component`.** The declarative-composition layer that hides imperative install behind render-tree reconciliation. Builds on this plan once the substrate is proven.
- **Shortcuts modal migration.** Validates the full pipeline once layering + reconciliation are in place.

## Risks called out

- **Cascade depth under deep nesting.** Each level of mount resolution doesn't add cascade depth, but each level of reactivity does (write → outer reactivity sub → outer tick → outer render → inner read updates → ...). At 3–4 nesting levels with auto-reactivity everywhere, the default cascade-bound of 64 is comfortable but a misconfigured component could exhaust it. Not addressed here; worth instrumenting when we see the first real multi-level UI.
- **Subscription registration order.** This plan registers in the order returned by `install_component`. For multi-component scenarios where one component's output feeds another's input via a shared mount, the producer must register before the consumer to be in the same cascade level. The shell is responsible for that ordering today (no framework helper). The Task 11 test relies on this implicitly — sub-shell's bundle is registered as one batch, in the order it was constructed, which is producer-first.
- **`MountTable::resolve` linear scan.** Fine at small N; not fine at hundreds of mounts. The plan doesn't index. Revisit if profiling shows it.
