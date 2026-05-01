//! Day-one broker subscription handlers for the settings screen.
//!
//! Each module here defines one `Subscription` impl that watches a path
//! pattern under `config/gate/accounts/` (or, for the save trigger,
//! `config/save`) and reacts by writing typed status / lifecycle records
//! and / or spawning network calls via [`crate::transport::Transport`].
//!
//! Registration is driven by [`register_all`] (added in N8); individual
//! subscriptions are exposed for tests and for direct registration when
//! callers want a curated subset.

pub mod account_create;
pub mod account_delete;
pub mod account_test;
pub mod catalog_refresh;
pub mod util;
