//! Pure interaction model shared by the Rust terminal dashboard.

use std::collections::VecDeque;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_INPUT_GRAPHEMES: usize = 4096;
const MAX_HISTORY: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleCommand {
    Help,
    Clear,
    Open,
    Quit,
}

impl ConsoleCommand {
    #[cfg(test)]
    fn name(self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::Clear => "clear",
            Self::Open => "open",
            Self::Quit => "quit",
        }
    }
}

pub fn parse_command(value: &str) -> Result<ConsoleCommand, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Enter a command. Type help to list available commands.".to_string());
    }
    let normalized = value
        .strip_prefix('/')
        .unwrap_or(value)
        .to_ascii_lowercase();
    match normalized.as_str() {
        "help" => Ok(ConsoleCommand::Help),
        "clear" => Ok(ConsoleCommand::Clear),
        "open" => Ok(ConsoleCommand::Open),
        "quit" | "q" => Ok(ConsoleCommand::Quit),
        _ => Err(format!(
            "Unknown command: {value}. Type help for available commands."
        )),
    }
}

#[derive(Debug, Default)]
pub struct InputEditor {
    value: String,
    cursor: usize,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft: String,
}

impl InputEditor {
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn cursor_cell(&self) -> usize {
        self.value
            .graphemes(true)
            .take(self.cursor)
            .map(UnicodeWidthStr::width)
            .sum()
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
    }

    pub fn insert_char(&mut self, value: char) {
        let mut text = String::new();
        text.push(value);
        self.insert(&text);
    }

    pub fn insert_paste(&mut self, value: &str) {
        let normalized = value
            .chars()
            .map(|character| match character {
                '\r' | '\n' | '\t' => ' ',
                value if value.is_control() => ' ',
                value => value,
            })
            .collect::<String>();
        self.insert(&normalized);
    }

    fn insert(&mut self, value: &str) {
        let current = self.value.graphemes(true).count();
        let available = MAX_INPUT_GRAPHEMES.saturating_sub(current);
        if available == 0 {
            return;
        }
        let value = value.graphemes(true).take(available).collect::<String>();
        let offset = grapheme_offset(&self.value, self.cursor);
        self.value.insert_str(offset, &value);
        self.cursor += value.graphemes(true).count();
        self.history_index = None;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.graphemes(true).count());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.value.graphemes(true).count();
    }

    pub fn set_cursor_from_cell(&mut self, cell: usize) {
        let mut width = 0;
        let mut cursor = 0;
        for grapheme in self.value.graphemes(true) {
            let next = width + UnicodeWidthStr::width(grapheme);
            if cell < next {
                break;
            }
            width = next;
            cursor += 1;
        }
        self.cursor = cursor;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = grapheme_offset(&self.value, self.cursor - 1);
        let end = grapheme_offset(&self.value, self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        let count = self.value.graphemes(true).count();
        if self.cursor >= count {
            return;
        }
        let start = grapheme_offset(&self.value, self.cursor);
        let end = grapheme_offset(&self.value, self.cursor + 1);
        self.value.replace_range(start..end, "");
    }

    pub fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = self.value.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.value = self.history[index].clone();
        self.move_end();
    }

    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.value = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.value = std::mem::take(&mut self.draft);
        }
        self.move_end();
    }

    pub fn submit(&mut self) -> Result<ConsoleCommand, String> {
        let submitted = self.value.trim().to_string();
        let parsed = parse_command(&submitted);
        if !submitted.is_empty() && self.history.back() != Some(&submitted) {
            if self.history.len() == MAX_HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(submitted);
        }
        self.clear();
        parsed
    }
}

fn grapheme_offset(value: &str, cursor: usize) -> usize {
    value
        .grapheme_indices(true)
        .nth(cursor)
        .map_or(value.len(), |(offset, _)| offset)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellPosition {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSelection {
    pub start: CellPosition,
    pub end: CellPosition,
}

impl ScreenSelection {
    pub fn ordered(self) -> (CellPosition, CellPosition) {
        if (self.start.y, self.start.x) <= (self.end.y, self.end.x) {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    pub fn contains(self, x: u16, y: u16) -> bool {
        let (start, end) = self.ordered();
        if y < start.y || y > end.y {
            return false;
        }
        (y != start.y || x >= start.x) && (y != end.y || x <= end.x)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScreenSnapshot {
    pub width: u16,
    pub height: u16,
    pub rows: Vec<Vec<String>>,
    pub input_y: u16,
    pub input_x: u16,
}

impl ScreenSnapshot {
    pub fn extract(&self, selection: ScreenSelection) -> String {
        let (start, end) = selection.ordered();
        if start == end || self.rows.is_empty() {
            return String::new();
        }
        let mut lines = Vec::new();
        for y in start.y..=end.y.min(self.height.saturating_sub(1)) {
            let Some(row) = self.rows.get(y as usize) else {
                continue;
            };
            let from = if y == start.y { start.x } else { 0 } as usize;
            let to = if y == end.y {
                end.x.min(self.width.saturating_sub(1)) as usize
            } else {
                self.width.saturating_sub(1) as usize
            };
            let mut line = row
                .iter()
                .skip(from)
                .take(to.saturating_sub(from) + 1)
                .map(String::as_str)
                .collect::<String>();
            while line.ends_with(' ') {
                line.pop();
            }
            lines.push(line);
        }
        lines.join("\n").trim_end_matches('\n').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_command_contract_is_implemented() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/terminal-console-contract.json"
        ))
        .unwrap();
        for case in contract["commands"].as_array().unwrap() {
            let parsed = parse_command(case["input"].as_str().unwrap()).unwrap();
            assert_eq!(parsed.name(), case["command"].as_str().unwrap());
        }
        for value in contract["invalid"].as_array().unwrap() {
            assert!(parse_command(value.as_str().unwrap()).is_err());
        }
    }

    #[test]
    fn editor_handles_graphemes_paste_and_history() {
        let mut editor = InputEditor::default();
        editor.insert_paste("/help\n你好👨‍👩‍👧‍👦");
        assert_eq!(editor.value(), "/help 你好👨‍👩‍👧‍👦");
        editor.backspace();
        assert_eq!(editor.value(), "/help 你好");
        editor.clear();
        editor.insert_paste("help");
        assert_eq!(editor.submit().unwrap(), ConsoleCommand::Help);
        editor.history_previous();
        assert_eq!(editor.value(), "help");
        editor.history_next();
        assert_eq!(editor.value(), "");
    }

    #[test]
    fn selection_extracts_forward_reverse_and_trims_padding() {
        let snapshot = ScreenSnapshot {
            width: 6,
            height: 2,
            rows: vec![
                vec!["a", "你", "", " ", " ", " "]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                vec!["b", "c", " ", " ", " ", " "]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ],
            input_y: 0,
            input_x: 0,
        };
        let selection = ScreenSelection {
            start: CellPosition { x: 1, y: 1 },
            end: CellPosition { x: 0, y: 0 },
        };
        assert_eq!(snapshot.extract(selection), "a你\nbc");
    }
}
