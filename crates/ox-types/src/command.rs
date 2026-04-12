use serde::{Deserialize, Serialize};

use crate::ui::InsertContext;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum UiCommand {
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    Open { thread_id: String },
    Close,
    GoToSettings,
    GoToInbox,
    EnterInsert { context: InsertContext },
    ExitInsert,
    SetInput { content: String, cursor: usize },
    ClearInput,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    ScrollPageUp,
    ScrollPageDown,
    ScrollHalfPageUp,
    ScrollHalfPageDown,
    SetScrollMax { max: usize },
    SetViewportHeight { height: usize },
    SetRowCount { count: usize },
    SendInput,
    Quit,
    OpenSelected,
    ArchiveSelected,
    ClearPendingAction,
    SearchInsertChar { char: char },
    SearchDeleteChar,
    SearchClear,
    SearchSaveChip,
    SearchDismissChip { index: usize },
}
