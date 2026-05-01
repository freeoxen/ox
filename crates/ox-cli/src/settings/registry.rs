//! Renderer registry — dispatches the current cursor to a `Renderer`,
//! returning a `View`. The translator (`crate::view_render`) draws the
//! View. The registry is `ox-cli`-local — it doesn't cross the broker
//! boundary.
//!
//! Design (per spec §4.3):
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

use ratatui::layout::Rect;
use structfs_core_store::{Path, Reader};

use ox_view::View;

use crate::theme::Theme;

/// What happens when a renderer is asked to ascend (Esc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AscendRule {
    /// Walk the display-tree parent chain until a registered renderer matches.
    /// Used by all detail/list pages: Esc returns to the nearest registered
    /// ancestor.
    NearestRegistered,
    /// Top-level page; ascending exits the settings screen entirely.
    /// Used by `settings/index`.
    ExitScreen,
}

/// A renderer is a pure function from a `Reader` to a `View`. It cannot
/// draw, await, or mutate. The output `View` is later turned into draw
/// calls by `crate::view_render::render_to_frame`.
///
/// The `'static` bound is implicit on `Box<dyn Renderer>`; renderers are
/// owned by the registry and live as long as the screen does.
pub trait Renderer: Send + Sync {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View;
    fn ascend_to(&self) -> AscendRule;
}

/// Context passed to `Renderer::render`. Provides the current draw area,
/// a Reader for snapshot reads, the registry itself for recursive
/// composition (modal-over-page renderers), and the theme.
///
/// **Mutability deviation from spec:** the spec quoted
/// `data: &'a dyn Reader` (immutable), but `Reader::read(&mut self, ...)`
/// requires a mutable reference to its receiver. We therefore hold
/// `&'a mut dyn Reader`. Renderers are still pure with respect to
/// observable application state — the mutation is internal to the
/// Reader (e.g. lazy-decode caches in `LiveReader`/`LocalConfig`).
pub struct RenderCtx<'a> {
    pub area:     Rect,
    pub data:     &'a mut dyn Reader,
    pub registry: &'a RendererRegistry,
    pub theme:    &'a Theme,
}

/// Indexes registered renderers by cursor `Path`.
///
/// - `lookup(cursor)` is exact-match.
/// - `render(cursor, ctx)` falls back to `View::unknown_cursor_fallback`
///   on miss.
/// - `ascend(cursor)` walks the display-tree parent chain per the
///   matched renderer's `AscendRule`. Returns `None` to signal
///   "exit the settings screen" (either `ExitScreen` rule, or no
///   registered ancestor exists for `NearestRegistered`).
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

    /// Render the page at `cursor`. On miss, returns the fallback View.
    pub fn render(&self, cursor: &Path, ctx: &mut RenderCtx<'_>) -> View {
        match self.specs.get(cursor) {
            Some(r) => r.render(ctx),
            None => View::unknown_cursor_fallback(cursor),
        }
    }

    /// Compute the cursor's "ascent" target per the matched renderer's
    /// rule. Returns `None` when there's no registered ancestor (i.e.
    /// the rule is `ExitScreen`, OR the rule is `NearestRegistered` and
    /// no ancestor up to the root is registered).
    pub fn ascend(&self, cursor: &Path) -> Option<Path> {
        let renderer = self.specs.get(cursor)?;
        match renderer.ascend_to() {
            AscendRule::ExitScreen => None,
            AscendRule::NearestRegistered => self.nearest_registered_parent(cursor),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ox_path::oxpath;
    use ox_store_util::local_config::LocalConfig;

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
            self.ascend
        }
    }

    fn fake(rule: AscendRule) -> Box<dyn Renderer> {
        Box::new(FakeRenderer { ascend: rule })
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
        let theme = Theme::default();
        let mut reader = LocalConfig::default();

        let cursor = oxpath!("settings", "accounts");
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: &mut reader,
            registry: &reg,
            theme: &theme,
        };

        let view = reg.render(&cursor, &mut ctx);
        assert_eq!(view, View::unknown_cursor_fallback(&cursor));
    }

    #[test]
    fn render_hit_invokes_registered_renderer() {
        let mut reg = RendererRegistry::new();
        reg.register(
            oxpath!("settings", "accounts"),
            fake(AscendRule::NearestRegistered),
        );
        let theme = Theme::default();
        let mut reader = LocalConfig::default();

        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: &mut reader,
            registry: &reg,
            theme: &theme,
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
