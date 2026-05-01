//! Curated, ratatui-agnostic View tree.
//!
//! `View` is the typed output of every renderer. It is intentionally a
//! *curated* widget set: anything outside requires extending this enum **and**
//! the translator. That cost is the point — it forces the design to stay
//! coherent.
//!
//! Hygiene:
//! - No `serde`, no `ratatui`. `View` is in-memory only in v1; the renderer
//!   produces it and the translator consumes it on the same machine.
//! - Every type derives `Debug, Clone, PartialEq` so renderer tests can
//!   compare with `assert_eq!` (struct equality is the assertion primitive).
//! - `Default` is added only where it directly aids ergonomic construction
//!   (`Style`, `Padding`, `ModifierSet`).

use structfs_core_store::Path;

// ---------------------------------------------------------------------------
// View enum
// ---------------------------------------------------------------------------

/// The curated widget set.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Empty,
    Text {
        spans: Vec<Span>,
        align: Align,
    },
    Stack {
        dir: Direction,
        children: Vec<(View, Sizing)>,
    },
    List {
        title: Option<String>,
        items: Vec<ListItem>,
        selected: Option<usize>,
    },
    Form {
        title: Option<String>,
        rows: Vec<FormRow>,
        focused: Option<usize>,
    },
    Modal {
        background: Box<View>,
        foreground: Box<View>,
        dim: bool,
    },
    Banner {
        kind: BannerKind,
        content: String,
    },
    /// Scrollable status block. `scroll_offset` is the visible-window offset
    /// the renderer reads from a path and threads through; the translator
    /// uses it to position the visible window. Carrying the offset (rather
    /// than a `scrollable: bool`) keeps the translator stateless.
    StatusBlock {
        title: String,
        lines: Vec<StyledLine>,
        scroll_offset: u16,
    },
    Pad {
        padding: Padding,
        child: Box<View>,
    },
}

// ---------------------------------------------------------------------------
// Sub-types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub primary: String,
    pub secondary: Option<String>,
    pub badge: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormRow {
    pub label: String,
    pub value: FormValue,
    pub error: Option<String>,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormValue {
    Text {
        value: String,
        cursor: u32,
        masked: bool,
    },
    Selector {
        options: Vec<String>,
        current: usize,
    },
    ReadOnly(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BannerKind {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    /// A span with no styling applied.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StyledLine(pub Vec<Span>);

#[derive(Debug, Clone, PartialEq)]
pub enum Direction {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Sizing {
    Fill,
    Fixed(u16),
    Min(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Padding {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: ModifierSet,
}

/// Color vocabulary, mirroring the standard ANSI/ratatui palette but kept
/// independent of any renderer crate. The translator maps these into ratatui
/// `Color` values; other renderers (e.g. a future Rio target) map them into
/// their own equivalents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Text modifier flags. `Default` is "no modifiers".
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModifierSet {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub reversed: bool,
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl View {
    /// A left-aligned single-span text view with default styling.
    pub fn text(s: impl Into<String>) -> View {
        View::Text {
            spans: vec![Span::plain(s)],
            align: Align::Left,
        }
    }

    /// A vertical stack.
    pub fn stack_v(children: Vec<(View, Sizing)>) -> View {
        View::Stack {
            dir: Direction::Vertical,
            children,
        }
    }

    /// A horizontal stack.
    pub fn stack_h(children: Vec<(View, Sizing)>) -> View {
        View::Stack {
            dir: Direction::Horizontal,
            children,
        }
    }

    /// Wrap `view` in `Pad { padding, child }`.
    pub fn pad(view: View, padding: Padding) -> View {
        View::Pad {
            padding,
            child: Box::new(view),
        }
    }

    /// Fallback view for an unknown cursor: an error banner naming the
    /// cursor plus an instruction line. Used by the renderer registry when
    /// no renderer claims a cursor path.
    pub fn unknown_cursor_fallback(cursor: &Path) -> View {
        let cursor_str = format!("{}", cursor);
        View::stack_v(vec![
            (
                View::Banner {
                    kind: BannerKind::Error,
                    content: format!("unknown cursor: {}", cursor_str),
                },
                Sizing::Min(1),
            ),
            (
                View::text("press Esc to return to the previous screen"),
                Sizing::Fill,
            ),
        ])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_a_text_view_and_compare_for_equality() {
        let v = View::text("hello");
        let expected = View::Text {
            spans: vec![Span {
                text: "hello".into(),
                style: Style::default(),
            }],
            align: Align::Left,
        };
        assert_eq!(v, expected);
    }
}
