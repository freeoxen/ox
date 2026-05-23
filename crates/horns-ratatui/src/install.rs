//! Install the ratatui backend as a broker mount.
//!
//! `install` registers a `ViewRenderSubscription` that watches the
//! configured view-input path. On every write, the subscription locks
//! the shared `Terminal` and calls `render_to_frame(view, frame, area,
//! theme)` — the horns instance owns the terminal for its frames; the
//! host's own `terminal.draw` calls must not race (the host's event
//! loop skips its draw call for any frame where horns owns the
//! screen).
//!
//! The terminal is shared via `Arc<parking_lot::Mutex<...>>` between
//! the host and this subscription. The screen-handoff contract
//! (who draws when) is the host's responsibility — typically a check
//! on `current screen == <horns-owned screen>`. When that check is
//! true the host skips its draw; when false the subscription's
//! `handle` is a no-op because the cursor doesn't sit on a
//! horns-registered renderer.
//!
//! The backend type is a parameter on `install`: production hosts
//! pass a `Terminal<CrosstermBackend<Stdout>>`; tests pass a
//! `Terminal<TestBackend>` and read rendered cells from the backend's
//! buffer. The subscription handler is identical either way — every
//! `Backend` impl is what `Terminal::draw` calls into.

use std::sync::Arc;

use horns_core::subscription::{PathPattern, SubCtx, Subscription, SubscriptionId};
use horns_core::view::View;
use horns_core::write::Write;
use parking_lot::Mutex;
use ratatui::Terminal;
use ratatui::backend::Backend;
use structfs_core_store::Path;

use crate::Theme;
use crate::render::render_to_frame;

/// Knobs the host passes into `install`.
pub struct RatatuiOptions<B: Backend + Send + 'static> {
    /// Path the horns framework writes the serialized `View` to. The
    /// subscription watches this; on write it locks the terminal and
    /// draws the View.
    pub view_input_path: Path,
    /// Shared terminal handle. The host (event loop) and this
    /// subscription both lock it via `parking_lot::Mutex` — the host
    /// for non-horns-owned frames, the subscription for horns-owned
    /// frames. The screen-handoff contract is the host's job to
    /// enforce.
    pub terminal: Arc<Mutex<Terminal<B>>>,
    /// Theme passed through to `render_to_frame`. Owned by the
    /// subscription (a clone is held); future revisions can read theme
    /// from a broker path if live theme-swap matters.
    pub theme: Theme,
}

/// Handle returned to the host. Holds the subscription id for
/// future supersession / teardown semantics.
pub struct RatatuiHandle {
    pub subscription_id: SubscriptionId,
}

/// Register the ratatui view-render subscription on `broker`.
pub fn install<B>(broker: &ox_broker::BrokerStore, opts: RatatuiOptions<B>) -> RatatuiHandle
where
    B: Backend + Send + 'static,
{
    let sub = ViewRenderSubscription::<B> {
        id: SubscriptionId("horns_ratatui.view_render".to_string()),
        watches: vec![PathPattern::Exact(opts.view_input_path.clone())],
        view_input_path: opts.view_input_path,
        terminal: opts.terminal,
        theme: opts.theme,
    };
    let id = sub.id.clone();
    broker.register_subscription(Arc::new(sub));
    RatatuiHandle {
        subscription_id: id,
    }
}

/// Subscription that locks the terminal and draws on every View write.
struct ViewRenderSubscription<B: Backend + Send + 'static> {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    view_input_path: Path,
    terminal: Arc<Mutex<Terminal<B>>>,
    theme: Theme,
}

impl<B: Backend + Send + 'static> Subscription for ViewRenderSubscription<B> {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        // Decode the View from the change's `after` record. The
        // `RenderSubscription` in horns-core writes the serialized
        // View; we deserialize and draw it.
        let Some(after) = &ctx.change.after else {
            return Vec::new();
        };
        let Some(value) = after.as_value() else {
            return Vec::new();
        };
        let Ok(view) = structfs_serde_store::from_value::<View>(value.clone()) else {
            tracing::warn!(
                path = %self.view_input_path,
                "horns_ratatui: View decode failed",
            );
            return Vec::new();
        };

        // Lock the terminal and draw. The host's contract: when the
        // horns-owned screen is active, the host's own `terminal.draw`
        // must not run — they'd race on the lock and one would
        // overwrite the other's frame.
        let mut term = self.terminal.lock();
        if let Err(e) = term.draw(|frame| {
            let area = frame.area();
            render_to_frame(&view, frame, area, &self.theme);
        }) {
            tracing::error!(error = %e, "horns_ratatui: terminal.draw failed");
        }

        Vec::new()
    }
}
