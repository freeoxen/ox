//! Renderer registry: cursor path -> `Box<dyn Renderer>`.
//!
//! - A renderer is a *pure function* from a Reader to a View. It cannot
//!   draw, await, or mutate observable state.
//! - The registry indexes renderers by exact cursor `Path`. On a miss,
//!   `render` returns `View::unknown_cursor_fallback(cursor)`.
//! - Esc-handling lives here: `ascend(cursor)` walks the display-tree
//!   parent chain per the matched renderer's `AscendRule`.
//! - Composition is value-shaped: a modal-over-page renderer constructs
//!   its View by recursively asking the registry for the parent View
//!   and wrapping it in `View::Modal { background, foreground, dim }`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Reader};

use crate::path_serde;
use crate::view::View;

/// Drawable region inside which a renderer produces its View. Mirrors
/// ratatui's `Rect` shape so backends with that abstraction can convert
/// at the boundary; horns-core stays backend-agnostic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// What happens when a renderer is asked to ascend (Esc).
///
/// Three variants because top-level pages within a screen don't fit
/// either of the two alternatives cleanly: their parent in the *display*
/// tree (the screen's index) is a sibling under the screen root, not a
/// strict ancestor. `Fallback(Path)` lets the renderer name its ascent
/// target explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AscendRule {
    /// Walk the display-tree parent chain until a registered renderer
    /// matches. Used by all detail/list pages: Esc returns to the
    /// nearest registered ancestor.
    NearestRegistered,
    /// Top-level page within a screen: ascend to the named cursor
    /// (typically the screen's index page). The named target must be a
    /// registered cursor; if it isn't, the registry falls through to
    /// `None` and the dispatcher signals `_request_exit` (same as
    /// `ExitScreen`).
    Fallback(#[serde(with = "path_serde")] Path),
    /// Top-level page; ascending exits the settings screen entirely.
    /// Used by `settings/index`.
    ExitScreen,
}

/// Context passed to `Renderer::render`. Provides the current draw area,
/// a Reader for snapshot reads, the registry itself for recursive
/// composition (modal-over-page renderers), and a host-defined theme.
///
/// **Theme is `&dyn Any`:** horns-core has no concrete Theme type.
/// Backends downcast at use site, or read theme bits from the broker
/// via the snapshot. Revisit if downcasting proves painful.
///
/// **Mutability deviation from spec:** the spec quoted
/// `data: &'a dyn Reader` (immutable), but `Reader::read(&mut self, ...)`
/// requires a mutable reference to its receiver. We therefore hold
/// `&'a mut dyn Reader`. Renderers are still pure with respect to
/// observable application state — the mutation is internal to the
/// Reader (e.g. lazy-decode caches in `LiveReader`/`LocalConfig`).
pub struct RenderCtx<'a> {
    pub area: Rect,
    pub data: &'a mut dyn Reader,
    pub registry: &'a RendererRegistry,
    pub theme: &'a dyn std::any::Any,
}

/// A renderer is a pure function from a `Reader` to a `View`. It cannot
/// draw, await, or mutate. The output `View` is later turned into draw
/// calls by a backend (`horns-ratatui`, etc.).
///
/// The `'static` bound is implicit on `Box<dyn Renderer>`; renderers are
/// owned by the registry and live as long as the screen does.
pub trait Renderer: Send + Sync {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View;
    fn ascend_to(&self) -> AscendRule;
}

/// Metadata stored at broker paths for a registered renderer. The
/// authoring view of a renderer — its ascend behavior — without the
/// Rust trait object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererMetadata {
    pub ascend_rule: AscendRule,
}

/// Indexes registered renderers by cursor `Path`.
///
/// - `lookup(cursor)` is exact-match.
/// - `render(cursor, ctx)` falls back to `View::unknown_cursor_fallback`
///   on miss.
/// - `ascend(cursor)` walks the display-tree parent chain per the
///   matched renderer's `AscendRule`. Returns `None` to signal
///   "exit the settings screen" (either `ExitScreen` rule, or no
///   registered ancestor exists for `NearestRegistered`, or a
///   `Fallback` whose target is unregistered).
pub struct RendererRegistry {
    specs: HashMap<Path, Box<dyn Renderer>>,
}

impl RendererRegistry {
    pub fn new() -> Self {
        Self {
            specs: HashMap::new(),
        }
    }

    /// Register `renderer` at `cursor`. Replaces any existing entry.
    pub fn register(&mut self, cursor: Path, renderer: Box<dyn Renderer>) {
        self.specs.insert(cursor, renderer);
    }

    /// Look up the renderer at `cursor`. Returns `None` if no exact match.
    pub fn lookup(&self, cursor: &Path) -> Option<&dyn Renderer> {
        self.specs.get(cursor).map(|b| b.as_ref())
    }

    /// Render the page at `cursor`. Walks the cursor's ancestors outer-
    /// to-inner to find the innermost registered renderer — the "page"
    /// the user is on is implicit in the cursor's ancestry, with no
    /// second cursor state path. Returns the fallback View only if
    /// neither the cursor nor any ancestor is registered.
    pub fn render(&self, cursor: &Path, ctx: &mut RenderCtx<'_>) -> View {
        if let Some(r) = self.specs.get(cursor) {
            return r.render(ctx);
        }
        if let Some(parent) = self.nearest_registered_parent(cursor) {
            return self.specs[&parent].render(ctx);
        }
        View::unknown_cursor_fallback(cursor)
    }

    /// Innermost registered ancestor of `cursor`, including `cursor`
    /// itself if it's registered. `None` if no ancestor (or self) is
    /// registered. Lets host-side commands ask the same question
    /// `render` asks (e.g. `nav.ascend` needs the page-level ancestor
    /// of a deeply-focused compound widget).
    pub fn registered_ancestor_or_self(&self, cursor: &Path) -> Option<Path> {
        if self.specs.contains_key(cursor) {
            return Some(cursor.clone());
        }
        self.nearest_registered_parent(cursor)
    }

    /// Compute the cursor's "ascent" target per the matched renderer's
    /// rule. Returns `None` when there's no registered ancestor (i.e.
    /// the rule is `ExitScreen`, OR the rule is `NearestRegistered` and
    /// no ancestor up to the root is registered, OR the rule is
    /// `Fallback(target)` but `target` is not registered).
    pub fn ascend(&self, cursor: &Path) -> Option<Path> {
        let renderer = self.specs.get(cursor)?;
        match renderer.ascend_to() {
            AscendRule::ExitScreen => None,
            AscendRule::NearestRegistered => self.nearest_registered_parent(cursor),
            AscendRule::Fallback(target) => {
                if self.specs.contains_key(&target) {
                    Some(target)
                } else {
                    None
                }
            }
        }
    }

    /// Walk strict ancestors of `cursor` (longest first) and return the
    /// first that has a registered renderer. Returns `None` if no
    /// strict ancestor is registered (including when `cursor` is the
    /// empty path).
    fn nearest_registered_parent(&self, cursor: &Path) -> Option<Path> {
        let mut len = cursor.len();
        while len > 0 {
            len -= 1;
            let candidate = cursor.slice(0, len);
            if self.specs.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
}

impl Default for RendererRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use ox_path::oxpath;
    use structfs_core_store::{Error, Record};

    use super::*;

    /// Minimal in-process Reader stub used by tests. Pulling
    /// `LocalConfig` from `ox-store-util` would create a dep cycle, so
    /// the registry tests bring their own.
    struct EmptyReader;

    impl Reader for EmptyReader {
        fn read(&mut self, _from: &Path) -> Result<Option<Record>, Error> {
            Ok(None)
        }
    }

    /// Minimal `Renderer` stub used by registry tests. Records nothing
    /// beyond the configured `AscendRule`; `render` always returns
    /// `View::Empty`.
    struct FakeRenderer {
        ascend: AscendRule,
    }

    impl Renderer for FakeRenderer {
        fn render(&self, _ctx: &mut RenderCtx<'_>) -> View {
            View::Empty
        }
        fn ascend_to(&self) -> AscendRule {
            self.ascend.clone()
        }
    }

    fn fake(rule: AscendRule) -> Box<dyn Renderer> {
        Box::new(FakeRenderer { ascend: rule })
    }

    fn fake_with_fallback(target: Path) -> Box<dyn Renderer> {
        Box::new(FakeRenderer {
            ascend: AscendRule::Fallback(target),
        })
    }

    #[test]
    fn ascend_exit_screen_returns_none() {
        let mut reg = RendererRegistry::new();
        reg.register(oxpath!("settings", "index"), fake(AscendRule::ExitScreen));

        assert_eq!(reg.ascend(&oxpath!("settings", "index")), None);
    }

    #[test]
    fn ascend_nearest_registered_walks_to_parent() {
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "accounts"),
            fake(AscendRule::NearestRegistered),
        );
        reg.register(
            oxpath!("settings", "accounts", "_detail"),
            fake(AscendRule::NearestRegistered),
        );

        let parent = reg.ascend(&oxpath!("settings", "accounts", "_detail"));
        assert_eq!(parent, Some(oxpath!("settings", "accounts")));
    }

    #[test]
    fn ascend_skips_unregistered_intermediate() {
        let mut reg = RendererRegistry::new();
        // Register at the root and at the deep leaf, but NOT at the
        // intermediate `settings/models`. Ascend should skip past it.
        reg.register(oxpath!("settings"), fake(AscendRule::NearestRegistered));
        reg.register(
            oxpath!("settings", "models", "_detail"),
            fake(AscendRule::NearestRegistered),
        );

        let parent = reg.ascend(&oxpath!("settings", "models", "_detail"));
        assert_eq!(parent, Some(oxpath!("settings")));
    }

    #[test]
    fn ascend_nearest_registered_with_no_ancestor_returns_none() {
        // Defensive: NearestRegistered at the root with no registered
        // ancestor must not loop forever — it should return None.
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "orphan"),
            fake(AscendRule::NearestRegistered),
        );

        assert_eq!(reg.ascend(&oxpath!("settings", "orphan")), None);
    }

    #[test]
    fn ascend_unknown_cursor_returns_none() {
        // Cursor with no registered renderer at all → None (no rule to apply).
        let reg = RendererRegistry::new();
        assert_eq!(reg.ascend(&oxpath!("settings", "ghost")), None);
    }

    #[test]
    fn ascend_fallback_returns_named_target() {
        // Top-level page declares Fallback to settings/index — registry
        // returns the named target (no strict-ancestor walk).
        let mut reg = RendererRegistry::new();
        reg.register(oxpath!("settings", "index"), fake(AscendRule::ExitScreen));
        reg.register(
            oxpath!("settings", "accounts"),
            fake_with_fallback(oxpath!("settings", "index")),
        );
        assert_eq!(
            reg.ascend(&oxpath!("settings", "accounts")),
            Some(oxpath!("settings", "index")),
        );
    }

    #[test]
    fn ascend_fallback_target_must_be_registered_or_returns_none() {
        // Guard against typos: a Fallback whose target is not a registered
        // cursor falls through to None (dispatcher then signals _request_exit).
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "accounts"),
            fake_with_fallback(oxpath!("settings", "ghost")),
        );
        assert_eq!(reg.ascend(&oxpath!("settings", "accounts")), None);
    }

    #[test]
    fn lookup_misses_return_none() {
        let reg = RendererRegistry::new();
        assert!(reg.lookup(&oxpath!("settings", "accounts")).is_none());
    }

    #[test]
    fn lookup_hits_return_renderer() {
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "accounts"),
            fake(AscendRule::NearestRegistered),
        );

        let r = reg.lookup(&oxpath!("settings", "accounts"));
        assert!(r.is_some());
        assert_eq!(r.unwrap().ascend_to(), AscendRule::NearestRegistered);
    }

    #[test]
    fn render_unknown_cursor_returns_fallback_view() {
        let reg = RendererRegistry::new();
        // horns-core has no concrete Theme type; tests use `()` as a stub.
        let theme: () = ();
        let mut reader = EmptyReader;

        let cursor = oxpath!("settings", "accounts");
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: &mut reader,
            registry: &reg,
            theme: &theme as &dyn std::any::Any,
        };

        let view = reg.render(&cursor, &mut ctx);
        assert_eq!(view, View::unknown_cursor_fallback(&cursor));
    }

    #[test]
    fn render_at_descendant_uses_innermost_registered_ancestor() {
        // Renderer registered only at the page; cursor sits on a
        // compound-widget leaf below it. The walk must find the page
        // and run its renderer rather than falling back to "unknown".
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "accounts"),
            fake(AscendRule::NearestRegistered),
        );
        let theme: () = ();
        let mut reader = EmptyReader;

        let cursor = oxpath!("settings", "accounts", "_detail", "alpha");
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: &mut reader,
            registry: &reg,
            theme: &theme as &dyn std::any::Any,
        };

        let view = reg.render(&cursor, &mut ctx);
        // FakeRenderer returns View::Empty — proves the registered
        // ancestor's renderer fired (not the fallback).
        assert_eq!(view, View::Empty);
    }

    #[test]
    fn registered_ancestor_or_self_returns_self_when_registered() {
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "accounts"),
            fake(AscendRule::NearestRegistered),
        );
        assert_eq!(
            reg.registered_ancestor_or_self(&oxpath!("settings", "accounts")),
            Some(oxpath!("settings", "accounts")),
        );
    }

    #[test]
    fn registered_ancestor_or_self_walks_when_descendant() {
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "accounts"),
            fake(AscendRule::NearestRegistered),
        );
        assert_eq!(
            reg.registered_ancestor_or_self(&oxpath!(
                "settings", "accounts", "_detail", "alpha"
            )),
            Some(oxpath!("settings", "accounts")),
        );
    }

    #[test]
    fn render_hit_invokes_registered_renderer() {
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "accounts"),
            fake(AscendRule::NearestRegistered),
        );
        let theme: () = ();
        let mut reader = EmptyReader;

        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: &mut reader,
            registry: &reg,
            theme: &theme as &dyn std::any::Any,
        };

        let view = reg.render(&oxpath!("settings", "accounts"), &mut ctx);
        // FakeRenderer always returns View::Empty.
        assert_eq!(view, View::Empty);
    }

    #[test]
    fn register_replaces_existing_entry() {
        let mut reg = RendererRegistry::new();
        reg.register(oxpath!("settings"), fake(AscendRule::NearestRegistered));
        // Re-register at the same cursor with a different rule.
        reg.register(oxpath!("settings"), fake(AscendRule::ExitScreen));

        assert_eq!(
            reg.lookup(&oxpath!("settings")).unwrap().ascend_to(),
            AscendRule::ExitScreen,
        );
    }
}
