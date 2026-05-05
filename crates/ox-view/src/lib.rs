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
    /// A bordered/titled box around any inner View. Splits the
    /// "is-a-list-of-items" concept from the "is-displayed-in-a-titled-box"
    /// concept so the same framing applies uniformly to lists, forms,
    /// stacks, or anything else. `title_right` renders right-aligned on
    /// the same border line as `title` — for status indicators that
    /// shouldn't displace the main title.
    Frame {
        title: Option<String>,
        title_right: Option<String>,
        content: Box<View>,
    },
    List {
        items: Vec<ListItem>,
        selected: Option<usize>,
    },
    Form {
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
    /// When present, the translator renders this styled-span sequence
    /// in place of `primary`. Used by the settings tree to render
    /// inline selector carousels — `[prev_dim] current [next_dim]` —
    /// without inventing a new `View` variant.
    pub primary_spans: Option<Vec<Span>>,
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

    // Sanity: D1's original test, preserved as the canonical Text example.
    #[test]
    fn view_text_via_constructor() {
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

    #[test]
    fn view_empty_equals_empty() {
        assert_eq!(View::Empty, View::Empty);
    }

    #[test]
    fn view_stack_v_constructor() {
        let v = View::stack_v(vec![
            (View::text("a"), Sizing::Fill),
            (View::text("b"), Sizing::Fixed(3)),
        ]);
        let expected = View::Stack {
            dir: Direction::Vertical,
            children: vec![
                (View::text("a"), Sizing::Fill),
                (View::text("b"), Sizing::Fixed(3)),
            ],
        };
        assert_eq!(v, expected);
    }

    #[test]
    fn view_stack_h_constructor() {
        let v = View::stack_h(vec![
            (View::text("left"), Sizing::Min(4)),
            (View::text("right"), Sizing::Fill),
        ]);
        let expected = View::Stack {
            dir: Direction::Horizontal,
            children: vec![
                (View::text("left"), Sizing::Min(4)),
                (View::text("right"), Sizing::Fill),
            ],
        };
        assert_eq!(v, expected);
    }

    #[test]
    fn view_list_with_items() {
        let v = View::List {
            items: vec![
                ListItem {
                    primary: "personal".into(),
                    primary_spans: None,
                    secondary: Some("anthropic".into()),
                    badge: Some("default".into()),
                },
                ListItem {
                    primary: "work".into(),
                    primary_spans: None,
                    secondary: Some("openai".into()),
                    badge: None,
                },
            ],
            selected: Some(1),
        };
        let expected = View::List {
            items: vec![
                ListItem {
                    primary: "personal".into(),
                    primary_spans: None,
                    secondary: Some("anthropic".into()),
                    badge: Some("default".into()),
                },
                ListItem {
                    primary: "work".into(),
                    primary_spans: None,
                    secondary: Some("openai".into()),
                    badge: None,
                },
            ],
            selected: Some(1),
        };
        assert_eq!(v, expected);
    }

    #[test]
    fn view_frame_wraps_a_list_with_title_and_right_status() {
        let inner = View::List {
            items: vec![ListItem {
                primary: "personal".into(),
                primary_spans: None,
                secondary: None,
                badge: None,
            }],
            selected: Some(0),
        };
        let framed = View::Frame {
            title: Some("Settings".into()),
            title_right: Some("● unsaved".into()),
            content: Box::new(inner.clone()),
        };
        match framed {
            View::Frame {
                title,
                title_right,
                content,
            } => {
                assert_eq!(title.as_deref(), Some("Settings"));
                assert_eq!(title_right.as_deref(), Some("● unsaved"));
                assert_eq!(*content, inner);
            }
            other => panic!("expected View::Frame, got {other:?}"),
        }
    }

    #[test]
    fn view_form_with_focused_row() {
        let v = View::Form {
            rows: vec![
                FormRow {
                    label: "endpoint".into(),
                    value: FormValue::Text {
                        value: "https://api.example.com".into(),
                        cursor: 23,
                        masked: false,
                    },
                    error: None,
                    hint: Some("HTTPS URL".into()),
                },
                FormRow {
                    label: "api_key".into(),
                    value: FormValue::Text {
                        value: "sk-secret".into(),
                        cursor: 9,
                        masked: true,
                    },
                    error: Some("required".into()),
                    hint: None,
                },
                FormRow {
                    label: "scheme".into(),
                    value: FormValue::Selector {
                        options: vec!["bearer".into(), "apikey".into()],
                        current: 0,
                    },
                    error: None,
                    hint: None,
                },
                FormRow {
                    label: "id".into(),
                    value: FormValue::ReadOnly("personal".into()),
                    error: None,
                    hint: None,
                },
            ],
            focused: Some(1),
        };

        // Spot-check: the view equals itself after a clone (covers Clone +
        // PartialEq across every embedded variant).
        assert_eq!(v.clone(), v);

        // Inspect the focused row by destructuring.
        if let View::Form { focused, rows, .. } = &v {
            assert_eq!(*focused, Some(1));
            assert_eq!(rows.len(), 4);
        } else {
            panic!("expected Form");
        }
    }

    #[test]
    fn view_modal_composes_two_views() {
        // Non-trivial background: a vertical stack of two children.
        let background = View::stack_v(vec![
            (View::text("status"), Sizing::Min(1)),
            (
                View::Frame {
                    title: Some("accounts".into()),
                    title_right: None,
                    content: Box::new(View::List {
                        items: vec![ListItem {
                            primary: "personal".into(),
                            primary_spans: None,
                            secondary: None,
                            badge: None,
                        }],
                        selected: Some(0),
                    }),
                },
                Sizing::Fill,
            ),
        ]);

        // Non-trivial foreground: a Form with one row, framed with a title.
        let foreground = View::Frame {
            title: Some("edit".into()),
            title_right: None,
            content: Box::new(View::Form {
                rows: vec![FormRow {
                    label: "name".into(),
                    value: FormValue::Text {
                        value: "personal".into(),
                        cursor: 8,
                        masked: false,
                    },
                    error: None,
                    hint: None,
                }],
                focused: Some(0),
            }),
        };

        let v = View::Modal {
            background: Box::new(background.clone()),
            foreground: Box::new(foreground.clone()),
            dim: true,
        };

        let expected = View::Modal {
            background: Box::new(background),
            foreground: Box::new(foreground),
            dim: true,
        };
        assert_eq!(v, expected);
    }

    #[test]
    fn view_banner_info_and_error() {
        let info = View::Banner {
            kind: BannerKind::Info,
            content: "saved".into(),
        };
        let err = View::Banner {
            kind: BannerKind::Error,
            content: "failed".into(),
        };
        assert_eq!(
            info,
            View::Banner {
                kind: BannerKind::Info,
                content: "saved".into()
            }
        );
        assert_eq!(
            err,
            View::Banner {
                kind: BannerKind::Error,
                content: "failed".into()
            }
        );
        // The two banners are distinct because BannerKind differs.
        assert_ne!(info, err);
    }

    #[test]
    fn view_status_block_with_scroll_offset() {
        let v = View::StatusBlock {
            title: "log".into(),
            lines: vec![
                StyledLine(vec![Span::plain("line 1")]),
                StyledLine(vec![
                    Span::plain("prefix "),
                    Span {
                        text: "EMPHASIS".into(),
                        style: Style {
                            fg: Some(Color::Red),
                            bg: None,
                            modifiers: ModifierSet {
                                bold: true,
                                ..ModifierSet::default()
                            },
                        },
                    },
                ]),
            ],
            scroll_offset: 5,
        };
        let expected = View::StatusBlock {
            title: "log".into(),
            lines: vec![
                StyledLine(vec![Span::plain("line 1")]),
                StyledLine(vec![
                    Span::plain("prefix "),
                    Span {
                        text: "EMPHASIS".into(),
                        style: Style {
                            fg: Some(Color::Red),
                            bg: None,
                            modifiers: ModifierSet {
                                bold: true,
                                ..ModifierSet::default()
                            },
                        },
                    },
                ]),
            ],
            scroll_offset: 5,
        };
        assert_eq!(v, expected);
    }

    #[test]
    fn view_pad_via_constructor() {
        let v = View::pad(
            View::text("inside"),
            Padding {
                top: 1,
                right: 2,
                bottom: 1,
                left: 2,
            },
        );
        let expected = View::Pad {
            padding: Padding {
                top: 1,
                right: 2,
                bottom: 1,
                left: 2,
            },
            child: Box::new(View::text("inside")),
        };
        assert_eq!(v, expected);
    }

    #[test]
    fn unknown_cursor_fallback_renders_a_stack_with_error_banner() {
        let cursor = Path::parse("gate/accounts/missing").unwrap();
        let v = View::unknown_cursor_fallback(&cursor);

        // Structural shape check: Stack { Vertical, [Banner(Error, "..."),
        // Text("press Esc...")] }.
        match &v {
            View::Stack { dir, children } => {
                assert_eq!(*dir, Direction::Vertical);
                assert_eq!(children.len(), 2);
                match &children[0].0 {
                    View::Banner { kind, content } => {
                        assert_eq!(*kind, BannerKind::Error);
                        assert!(content.contains("unknown cursor"));
                        assert!(content.contains("gate/accounts/missing"));
                    }
                    other => panic!("expected Banner, got {other:?}"),
                }
                match &children[1].0 {
                    View::Text { spans, .. } => {
                        assert!(spans[0].text.contains("Esc"));
                    }
                    other => panic!("expected Text, got {other:?}"),
                }
            }
            other => panic!("expected Stack, got {other:?}"),
        }
    }
}
