use ratatui::style::{Color, Modifier, Style};

/// Semantic theme for the TUI. Every styled element references a named slot.
///
/// The default theme uses terminal-relative styling (DIM, BOLD, REVERSED)
/// rather than absolute ANSI colors where possible, so it adapts to any
/// terminal color scheme — solarized, gruvbox, dracula, default, etc.
///
/// Accent colors (Green, Yellow, Red) use the base ANSI palette which every
/// scheme defines to be readable against its own background.
pub struct Theme {
    /// Title bar badge (e.g. " ox ").
    pub title_badge: Style,
    /// Title bar model/provider text.
    pub title_info: Style,

    /// User prompt marker ("> ").
    pub user_prompt: Style,
    /// User message text.
    pub user_text: Style,

    /// Assistant response text.
    pub assistant_text: Style,

    /// Tool call name bracket (e.g. "[read_file]").
    pub tool_name: Style,
    /// Tool "running..." indicator.
    pub tool_running: Style,
    /// Tool result line count.
    pub tool_meta: Style,
    /// Tool result preview lines.
    pub tool_output: Style,
    /// Tool overflow indicator ("... N more").
    pub tool_overflow: Style,

    /// Thinking indicator ("...").
    pub thinking: Style,

    /// Error messages.
    pub error: Style,

    /// Input box border.
    pub input_border: Style,

    /// Status bar text.
    pub status: Style,

    /// Approval dialog border.
    pub approval_border: Style,
    /// Approval dialog title ("Permission Required").
    pub approval_title: Style,
    /// Approval dialog tool name.
    pub approval_tool: Style,
    /// Approval dialog input preview.
    pub approval_preview: Style,
    /// Approval dialog selected option.
    pub approval_selected: Style,
    /// Approval dialog unselected option.
    pub approval_option: Style,
}

impl Theme {
    /// Adaptive theme that works on any terminal color scheme.
    ///
    /// Uses DIM for de-emphasized text (instead of DarkGray which is
    /// invisible on solarized dark), REVERSED for the title badge
    /// (instead of hardcoded fg/bg), and ANSI accent colors for
    /// semantic highlights.
    pub fn default_theme() -> Self {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let bold = Style::default().add_modifier(Modifier::BOLD);

        Self {
            title_badge: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            title_info: dim,

            user_prompt: bold.fg(Color::Green),
            user_text: Style::default(),

            assistant_text: Style::default(),

            tool_name: bold.fg(Color::Yellow),
            tool_running: dim,
            tool_meta: dim,
            tool_output: dim,
            tool_overflow: dim,

            thinking: dim,

            error: bold.fg(Color::Red),

            input_border: dim,

            status: dim,

            approval_border: bold.fg(Color::Yellow),
            approval_title: bold.fg(Color::Yellow),
            approval_tool: bold,
            approval_preview: dim,
            approval_selected: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            approval_option: dim,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_theme()
    }
}
