//! StructFS pagination types — cursor-based, HATEOAS navigation.
//!
//! Implements the Page response structure from the StructFS pagination pattern.
//! Clients follow `links.next` to iterate; they never construct pagination URLs.

use std::collections::BTreeMap;
use structfs_core_store::Value;

/// A paginated response.
pub struct Page {
    /// Items in this page (inline values).
    pub items: Vec<Value>,
    /// Page metadata.
    pub page: PageInfo,
    /// Navigation links.
    pub links: PageLinks,
}

/// Metadata about the current page.
pub struct PageInfo {
    /// Number of items in this response.
    pub size: usize,
    /// Total items in collection (if known).
    pub total: Option<usize>,
}

/// Navigation links for pagination.
pub struct PageLinks {
    /// This page (for caching/bookmarking).
    pub self_link: String,
    /// Next page (absent on last page).
    pub next: Option<String>,
    /// Previous page (absent on first page).
    pub prev: Option<String>,
    /// First page.
    pub first: Option<String>,
}

impl Page {
    /// Check if this is the last page.
    pub fn is_last(&self) -> bool {
        self.links.next.is_none()
    }

    /// Serialize to a StructFS Value.
    pub fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("items".to_string(), Value::Array(self.items.clone()));
        map.insert("page".to_string(), self.page.to_value());
        map.insert("links".to_string(), self.links.to_value());
        Value::Map(map)
    }

    /// Parse from a StructFS Value.
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = match value {
            Value::Map(m) => m,
            _ => return None,
        };
        let items = match map.get("items") {
            Some(Value::Array(arr)) => arr.clone(),
            _ => return None,
        };
        let page = map.get("page").and_then(PageInfo::from_value)?;
        let links = map.get("links").and_then(PageLinks::from_value)?;
        Some(Page { items, page, links })
    }
}

impl PageInfo {
    pub fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("size".to_string(), Value::Integer(self.size as i64));
        if let Some(total) = self.total {
            map.insert("total".to_string(), Value::Integer(total as i64));
        }
        Value::Map(map)
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let map = match value {
            Value::Map(m) => m,
            _ => return None,
        };
        let size = match map.get("size") {
            Some(Value::Integer(n)) => *n as usize,
            _ => return None,
        };
        let total = match map.get("total") {
            Some(Value::Integer(n)) => Some(*n as usize),
            _ => None,
        };
        Some(PageInfo { size, total })
    }
}

impl PageLinks {
    pub fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("self".to_string(), ref_value(&self.self_link));
        if let Some(ref next) = self.next {
            map.insert("next".to_string(), ref_value(next));
        }
        if let Some(ref prev) = self.prev {
            map.insert("prev".to_string(), ref_value(prev));
        }
        if let Some(ref first) = self.first {
            map.insert("first".to_string(), ref_value(first));
        }
        Value::Map(map)
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let map = match value {
            Value::Map(m) => m,
            _ => return None,
        };
        let self_link = map.get("self").and_then(ref_path).unwrap_or_default();
        let next = map.get("next").and_then(ref_path);
        let prev = map.get("prev").and_then(ref_path);
        let first = map.get("first").and_then(ref_path);
        Some(PageLinks {
            self_link,
            next,
            prev,
            first,
        })
    }
}

/// Build a StructFS Reference value: `{"path": "..."}`.
fn ref_value(path: &str) -> Value {
    let mut map = BTreeMap::new();
    map.insert("path".to_string(), Value::String(path.to_string()));
    Value::Map(map)
}

/// Extract the path from a StructFS Reference value.
fn ref_path(value: &Value) -> Option<String> {
    let map = match value {
        Value::Map(m) => m,
        _ => return None,
    };
    match map.get("path") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Build a Page from a result set slice with cursor-based navigation.
///
/// `base_path` is the result set prefix (e.g. `search/results/0001`).
/// `cursor_field` is the key used for keyed cursors (e.g. `"id"`).
pub fn paginate(
    all_items: &[Value],
    base_path: &str,
    cursor_field: &str,
    after_cursor: Option<&str>,
    limit: usize,
) -> Page {
    let limit = limit.clamp(1, 100);
    let total = all_items.len();

    // Find start position
    let start = match after_cursor {
        Some(cursor) => {
            // Find the item with the cursor value, start after it
            all_items
                .iter()
                .position(|item| {
                    if let Value::Map(m) = item {
                        m.get(cursor_field) == Some(&Value::String(cursor.to_string()))
                    } else {
                        false
                    }
                })
                .map(|pos| pos + 1)
                .unwrap_or(total)
        }
        None => 0,
    };

    let end = (start + limit).min(total);
    let items: Vec<Value> = if start < total {
        all_items[start..end].to_vec()
    } else {
        Vec::new()
    };
    let size = items.len();

    // Build self link
    let self_link = match after_cursor {
        Some(c) => format!("{base_path}/after/{c}/limit/{limit}"),
        None => format!("{base_path}/limit/{limit}"),
    };

    // Build next link (absent on last page)
    let next = if end < total {
        // Cursor is the last item's ID
        items.last().and_then(|item| {
            if let Value::Map(m) = item {
                if let Some(Value::String(id)) = m.get(cursor_field) {
                    return Some(format!("{base_path}/after/{id}/limit/{limit}"));
                }
            }
            None
        })
    } else {
        None
    };

    // Build prev link (absent on first page)
    let prev = if start > 0 {
        items.first().and_then(|item| {
            if let Value::Map(m) = item {
                if let Some(Value::String(id)) = m.get(cursor_field) {
                    return Some(format!("{base_path}/before/{id}/limit/{limit}"));
                }
            }
            None
        })
    } else {
        None
    };

    let first = if start > 0 {
        Some(format!("{base_path}/limit/{limit}"))
    } else {
        None
    };

    Page {
        items,
        page: PageInfo {
            size,
            total: Some(total),
        },
        links: PageLinks {
            self_link,
            next,
            prev,
            first,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_item(id: &str, title: &str) -> Value {
        let mut map = BTreeMap::new();
        map.insert("id".to_string(), Value::String(id.to_string()));
        map.insert("title".to_string(), Value::String(title.to_string()));
        Value::Map(map)
    }

    #[test]
    fn paginate_first_page() {
        let items: Vec<Value> = (0..5)
            .map(|i| thread_item(&format!("t_{i}"), &format!("Thread {i}")))
            .collect();
        let page = paginate(&items, "search/results/0001", "id", None, 3);

        assert_eq!(page.page.size, 3);
        assert_eq!(page.page.total, Some(5));
        assert_eq!(page.items.len(), 3);
        assert!(page.links.next.is_some());
        assert!(page.links.prev.is_none());
        assert!(page.links.first.is_none()); // already on first page
        assert_eq!(page.links.self_link, "search/results/0001/limit/3");
        assert_eq!(
            page.links.next.unwrap(),
            "search/results/0001/after/t_2/limit/3"
        );
    }

    #[test]
    fn paginate_after_cursor() {
        let items: Vec<Value> = (0..5)
            .map(|i| thread_item(&format!("t_{i}"), &format!("Thread {i}")))
            .collect();
        let page = paginate(&items, "search/results/0001", "id", Some("t_2"), 3);

        assert_eq!(page.page.size, 2); // t_3, t_4
        assert_eq!(page.page.total, Some(5));
        assert!(page.links.next.is_none()); // last page
        assert!(page.links.prev.is_some());
        assert!(page.links.first.is_some());
    }

    #[test]
    fn paginate_empty_collection() {
        let items: Vec<Value> = Vec::new();
        let page = paginate(&items, "search/results/0001", "id", None, 20);

        assert_eq!(page.page.size, 0);
        assert_eq!(page.page.total, Some(0));
        assert!(page.items.is_empty());
        assert!(page.links.next.is_none());
        assert!(page.links.prev.is_none());
    }

    #[test]
    fn paginate_exact_fit() {
        let items: Vec<Value> = (0..3)
            .map(|i| thread_item(&format!("t_{i}"), &format!("Thread {i}")))
            .collect();
        let page = paginate(&items, "search/results/0001", "id", None, 3);

        assert_eq!(page.page.size, 3);
        assert!(page.links.next.is_none()); // exactly fills one page
        assert!(page.is_last());
    }

    #[test]
    fn paginate_invalid_cursor_returns_empty() {
        let items: Vec<Value> = (0..5)
            .map(|i| thread_item(&format!("t_{i}"), &format!("Thread {i}")))
            .collect();
        let page = paginate(&items, "search/results/0001", "id", Some("nonexistent"), 3);

        assert_eq!(page.page.size, 0);
        assert!(page.items.is_empty());
    }

    #[test]
    fn page_round_trip() {
        let items: Vec<Value> = (0..3)
            .map(|i| thread_item(&format!("t_{i}"), &format!("Thread {i}")))
            .collect();
        let page = paginate(&items, "search/results/0001", "id", None, 10);

        let value = page.to_value();
        let parsed = Page::from_value(&value).expect("should parse");
        assert_eq!(parsed.page.size, 3);
        assert_eq!(parsed.page.total, Some(3));
        assert_eq!(parsed.items.len(), 3);
        assert!(parsed.links.next.is_none());
        assert_eq!(parsed.links.self_link, "search/results/0001/limit/10");
    }

    #[test]
    fn limit_clamped_to_bounds() {
        let items: Vec<Value> = (0..5)
            .map(|i| thread_item(&format!("t_{i}"), &format!("Thread {i}")))
            .collect();
        // Limit 0 → clamped to 1
        let page = paginate(&items, "r", "id", None, 0);
        assert_eq!(page.page.size, 1);

        // Limit 999 → clamped to 100
        let page = paginate(&items, "r", "id", None, 999);
        assert_eq!(page.page.size, 5); // only 5 items exist
    }
}
