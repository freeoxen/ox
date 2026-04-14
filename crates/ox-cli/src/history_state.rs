//! History explorer state — LogCache (adapter) + HistoryLayout (layout manager).
//!
//! Follows the RecyclerView pattern:
//! - LogCache: incrementally caches parsed log entries, only parses new appends
//! - HistoryLayout: computes visible range, cursor-following scroll
//!
//! The event loop owns a HistoryExplorer and feeds it log count + entry data.
//! The renderer receives only the visible slice and layout state.

use crate::parse::{LogDisplayEntry, parse_log_entries};
use structfs_core_store::Value;

// ---------------------------------------------------------------------------
// LogCache — the Adapter
// ---------------------------------------------------------------------------

/// Incrementally cached parsed log entries.
///
/// On each frame, call `sync()` with the current log count and a fetch function.
/// Only new entries (appended since last sync) are parsed.
pub struct LogCache {
    entries: Vec<LogDisplayEntry>,
    /// Number of raw log values we've consumed (may differ from entries.len()
    /// if some values fail to parse).
    raw_count: usize,
}

impl LogCache {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            raw_count: 0,
        }
    }

    /// Sync the cache with the log. Only parses entries beyond `raw_count`.
    ///
    /// `all_raw` is the full raw values slice from the broker read.
    /// We only parse the tail that's new since last sync.
    pub fn sync(&mut self, all_raw: &[Value]) {
        let new_count = all_raw.len();
        if new_count <= self.raw_count {
            return;
        }

        let new_slice = &all_raw[self.raw_count..];
        let mut new_entries = parse_log_entries(new_slice);

        // Re-index: entries from parse_log_entries are indexed 0..N within the slice,
        // but we need global indices.
        let offset = self.entries.len();
        for entry in &mut new_entries {
            entry.index += offset;
        }

        // Duplicate detection: check the last new entry against prior entries
        // (parse_log_entries only detects duplicates within its own slice).
        if !self.entries.is_empty() && !new_entries.is_empty() {
            detect_cross_boundary_duplicates(&self.entries, &mut new_entries);
        }

        self.entries.extend(new_entries);
        self.raw_count = new_count;
    }

    /// Total number of parsed entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get entry by index.
    #[cfg(test)]
    pub fn get(&self, index: usize) -> Option<&LogDisplayEntry> {
        self.entries.get(index)
    }

    /// Get a slice of entries for the visible range.
    pub fn slice(&self, range: std::ops::Range<usize>) -> &[LogDisplayEntry] {
        let start = range.start.min(self.entries.len());
        let end = range.end.min(self.entries.len());
        &self.entries[start..end]
    }

    /// Reset the cache (e.g. when switching threads).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.raw_count = 0;
    }
}

/// Check first entries in `new` against the tail of `existing` for duplicates.
fn detect_cross_boundary_duplicates(existing: &[LogDisplayEntry], new: &mut [LogDisplayEntry]) {
    use crate::parse::concat_text;

    for entry in new.iter_mut() {
        let et = &entry.entry_type;
        if et != "user" && et != "assistant" {
            continue;
        }
        if entry.meta.text_len == 0 {
            continue;
        }
        // Look backwards in existing for the most recent same-role entry
        for prev in existing.iter().rev() {
            if prev.entry_type != entry.entry_type {
                continue;
            }
            let text_new = concat_text(&entry.blocks);
            let text_prev = concat_text(&prev.blocks);
            if !text_new.is_empty() && text_new == text_prev {
                entry.flags.duplicate_content = true;
                entry.flags.duplicate_of = Some(prev.index);
            }
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// HistoryLayout — the LayoutManager
// ---------------------------------------------------------------------------

/// Computes visible entry range and cursor-following scroll.
///
/// Scroll is in entry-index space (not pixel/line space). The first visible
/// entry is `scroll_offset`. The renderer renders entries from
/// `scroll_offset..scroll_offset + viewport_capacity`.
pub struct HistoryLayout {
    /// First visible entry index.
    scroll_offset: usize,
    /// How many entries fit in the viewport (approximate — collapsed entries
    /// are 1 row, expanded entries are more, but we use entry count not lines).
    viewport_capacity: usize,
}

impl HistoryLayout {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0,
            viewport_capacity: 0,
        }
    }

    /// Update viewport capacity from terminal height.
    /// Subtracts 2 for the header lines.
    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport_capacity = height.saturating_sub(2);
    }

    /// Ensure the selected row is visible, adjusting scroll_offset if needed.
    pub fn ensure_visible(&mut self, selected: usize) {
        if self.viewport_capacity == 0 {
            return;
        }
        if selected < self.scroll_offset {
            self.scroll_offset = selected;
        } else if selected >= self.scroll_offset + self.viewport_capacity {
            self.scroll_offset = selected - self.viewport_capacity + 1;
        }
    }

    /// Clamp scroll_offset to valid range given total entry count.
    pub fn clamp(&mut self, total: usize) {
        if total == 0 {
            self.scroll_offset = 0;
            return;
        }
        let max_offset = total.saturating_sub(self.viewport_capacity);
        if self.scroll_offset > max_offset {
            self.scroll_offset = max_offset;
        }
    }

    /// Range of entry indices to render.
    pub fn visible_range(&self, total: usize) -> std::ops::Range<usize> {
        let start = self.scroll_offset;
        let end = (self.scroll_offset + self.viewport_capacity).min(total);
        start..end
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }
}

// ---------------------------------------------------------------------------
// HistoryExplorer — combines Cache + Layout
// ---------------------------------------------------------------------------

/// Owns the full history explorer state. Lives in the event loop.
pub struct HistoryExplorer {
    pub cache: LogCache,
    pub layout: HistoryLayout,
    /// Thread ID this explorer is tracking. Cleared when switching threads.
    thread_id: Option<String>,
    /// Generation counter — incremented when visible state changes.
    /// The renderer can compare against its last rendered generation
    /// to skip re-rendering.
    generation: u64,
    last_count: usize,
    last_selected: usize,
    last_expanded_hash: u64,
}

impl HistoryExplorer {
    pub fn new() -> Self {
        Self {
            cache: LogCache::new(),
            layout: HistoryLayout::new(),
            thread_id: None,
            generation: 0,
            last_count: 0,
            last_selected: 0,
            last_expanded_hash: 0,
        }
    }

    /// Sync with the current frame's data. Returns true if anything changed
    /// (new entries, selection moved, expanded set changed).
    pub fn sync(
        &mut self,
        thread_id: &str,
        raw_values: &[Value],
        selected: usize,
        expanded: &[usize],
        viewport_height: usize,
    ) -> bool {
        // Thread changed — reset cache
        if self.thread_id.as_deref() != Some(thread_id) {
            self.cache.clear();
            self.thread_id = Some(thread_id.to_string());
        }

        // Incremental parse
        self.cache.sync(raw_values);

        // Layout
        self.layout.set_viewport_height(viewport_height);
        let clamped_selected = if self.cache.is_empty() {
            0
        } else {
            selected.min(self.cache.len() - 1)
        };
        self.layout.ensure_visible(clamped_selected);
        self.layout.clamp(self.cache.len());

        // Check if anything changed
        let expanded_hash = hash_expanded(expanded);
        let changed = self.cache.len() != self.last_count
            || clamped_selected != self.last_selected
            || expanded_hash != self.last_expanded_hash;

        if changed {
            self.generation += 1;
            self.last_count = self.cache.len();
            self.last_selected = clamped_selected;
            self.last_expanded_hash = expanded_hash;
        }

        changed
    }

    #[cfg(test)]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn entry_count(&self) -> usize {
        self.cache.len()
    }
}

fn hash_expanded(expanded: &[usize]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    expanded.hash(&mut hasher);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::Value;

    fn make_user_msg(content: &str) -> Value {
        let mut map = BTreeMap::new();
        map.insert("type".to_string(), Value::String("user".to_string()));
        map.insert("content".to_string(), Value::String(content.to_string()));
        Value::Map(map)
    }

    fn make_assistant_msg(content: &str) -> Value {
        let mut map = BTreeMap::new();
        map.insert("type".to_string(), Value::String("assistant".to_string()));
        map.insert("content".to_string(), Value::String(content.to_string()));
        Value::Map(map)
    }

    fn make_turn_start() -> Value {
        let mut map = BTreeMap::new();
        map.insert("type".to_string(), Value::String("turn_start".to_string()));
        Value::Map(map)
    }

    #[test]
    fn cache_incremental_sync() {
        let mut cache = LogCache::new();
        let v1 = vec![make_user_msg("hello")];
        cache.sync(&v1);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(0).unwrap().entry_type, "user");

        // Append more
        let v2 = vec![
            make_user_msg("hello"),
            make_assistant_msg("hi"),
            make_turn_start(),
        ];
        cache.sync(&v2);
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get(0).unwrap().index, 0);
        assert_eq!(cache.get(1).unwrap().index, 1);
        assert_eq!(cache.get(2).unwrap().index, 2);
    }

    #[test]
    fn cache_no_reparse_on_same_count() {
        let mut cache = LogCache::new();
        let v = vec![make_user_msg("hello")];
        cache.sync(&v);
        assert_eq!(cache.len(), 1);
        // Sync again with same data — nothing changes
        cache.sync(&v);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_cross_boundary_duplicate_detection() {
        let mut cache = LogCache::new();
        let v1 = vec![make_assistant_msg("same text")];
        cache.sync(&v1);
        assert!(!cache.get(0).unwrap().flags.duplicate_content);

        let v2 = vec![
            make_assistant_msg("same text"),
            make_user_msg("interlude"),
            make_assistant_msg("same text"),
        ];
        cache.sync(&v2);
        // Entry at index 2 is a duplicate of index 0
        assert!(cache.get(2).unwrap().flags.duplicate_content);
        assert_eq!(cache.get(2).unwrap().flags.duplicate_of, Some(0));
    }

    #[test]
    fn layout_cursor_following() {
        let mut layout = HistoryLayout::new();
        layout.set_viewport_height(12); // 10 entries visible (minus 2 header)

        // Selected at 0 — scroll stays at 0
        layout.ensure_visible(0);
        assert_eq!(layout.scroll_offset(), 0);

        // Selected at 15 — scrolls to show it
        layout.ensure_visible(15);
        assert_eq!(layout.scroll_offset(), 6); // 15 - 10 + 1

        // Selected at 5 — scrolls back
        layout.ensure_visible(5);
        assert_eq!(layout.scroll_offset(), 5);
    }

    #[test]
    fn layout_clamp() {
        let mut layout = HistoryLayout::new();
        layout.set_viewport_height(12); // capacity = 10
        layout.ensure_visible(50);
        assert_eq!(layout.scroll_offset(), 41);

        // Total only 5 entries — clamp to 0
        layout.clamp(5);
        assert_eq!(layout.scroll_offset(), 0);
    }

    #[test]
    fn layout_visible_range() {
        let mut layout = HistoryLayout::new();
        layout.set_viewport_height(12); // capacity = 10
        layout.ensure_visible(5);

        let range = layout.visible_range(20);
        assert_eq!(range, 0..10);

        layout.ensure_visible(15);
        let range = layout.visible_range(20);
        assert_eq!(range, 6..16);
    }

    #[test]
    fn explorer_detects_changes() {
        let mut explorer = HistoryExplorer::new();
        let v1 = vec![make_user_msg("hello")];

        let changed = explorer.sync("t1", &v1, 0, &[], 12);
        assert!(changed);
        assert_eq!(explorer.generation(), 1);

        // Same state — no change
        let changed = explorer.sync("t1", &v1, 0, &[], 12);
        assert!(!changed);
        assert_eq!(explorer.generation(), 1);

        // New entry — changes
        let v2 = vec![make_user_msg("hello"), make_assistant_msg("hi")];
        let changed = explorer.sync("t1", &v2, 0, &[], 12);
        assert!(changed);
        assert_eq!(explorer.generation(), 2);

        // Selection moved — changes
        let changed = explorer.sync("t1", &v2, 1, &[], 12);
        assert!(changed);
        assert_eq!(explorer.generation(), 3);
    }

    #[test]
    fn explorer_clears_on_thread_switch() {
        let mut explorer = HistoryExplorer::new();
        let v1 = vec![make_user_msg("hello")];
        explorer.sync("t1", &v1, 0, &[], 12);
        assert_eq!(explorer.entry_count(), 1);

        let v2 = vec![make_user_msg("different thread")];
        explorer.sync("t2", &v2, 0, &[], 12);
        assert_eq!(explorer.entry_count(), 1);
        assert_eq!(explorer.cache.get(0).unwrap().summary, "different thread");
    }
}
