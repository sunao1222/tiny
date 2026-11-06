use std::mem;

use ratatui::buffer::Buffer;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::config::{Colors, Style};
use crate::key_map::KeyAction;
use crate::line_split::LineDataCache;
use crate::msg_area::MsgArea;
use crate::text;
use crate::trie::Trie;
use crate::utils;
use crate::widget::WidgetRet;

pub(crate) mod input_line;
use self::input_line::{InputLine, draw_line, draw_line_autocomplete};

/// Minimal number of terminal columns to keep around the cursor when scrolling.
const SCROLL_OFF: i32 = 5;

/// Minimum width of InputArea for wrapping
const SCROLL_FALLBACK_WIDTH: i32 = 36;

/// Input history size
const HIST_SIZE: usize = 30;

pub(crate) struct InputArea {
    /// The message that's currently being edited (not yet sent)
    buffer: InputLine,

    /// Cursor position
    cursor: i32,

    /// Width of the widget
    width: i32,

    /// Height of the widget, invalidated on input and resize
    height: Option<i32>,

    /// Maximum lines the widget can grow to
    max_lines: i32,

    /// Amount of scroll in the input field
    scroll: Option<i32>,

    /// A history of sent messages/commands. Once added messages are never
    /// modified. A modification attempt should result in a new buffer with a
    /// copy of the vector in history. (old contents of the buffer will be lost)
    history: Vec<InputLine>,

    mode: Mode,

    /// Current nickname. Not available on initialization (e.g. before registration with the
    /// server). Set with `set_nick`.
    nick: Option<Nickname>,
}

enum Mode {
    /// Editing the buffer
    Edit,

    /// Browsing history
    History(i32),

    /// Auto-completing a command or a nick in a channel
    Autocomplete {
        original_buffer: InputLine,
        insertion_point: usize,
        word_starts: usize,
        completions: Vec<String>,
        current_completion: usize,
    },
}

pub(crate) struct Nickname {
    value: String,
    color: usize,
}

static NICKNAME_SUFFIX: &str = ": ";

impl Nickname {
    fn new(value: String, color: usize) -> Nickname {
        Nickname { value, color }
    }

    /// Calculates the length of the nickname based on given width, including the NICKNAME_SUFFIX.
    /// Width should be the width of the `InputArea`. When length of the nick is 30% or less of the
    /// `TextField` width we show it (returns width of the nick), otherwise we don't (returns 0).
    fn len(&self, input_area_width: i32) -> usize {
        let len = text::text_width(&self.value) + text::text_width(NICKNAME_SUFFIX);
        if len as f32 <= input_area_width as f32 * (30f32 / 100f32) {
            len
        } else {
            0
        }
    }

    fn draw(&self, tb: &mut Buffer, colors: &Colors, mut pos_x: i32, pos_y: i32, width: i32) {
        if self.len(width) > 0 {
            let nick_color = colors.nick[self.color % colors.nick.len()];
            let style = Style {
                fg: if nick_color == 0 {
                    ratatui::style::Color::Reset
                } else {
                    ratatui::style::Color::Indexed(nick_color)
                },
                bg: colors.user_msg.bg,
                modifiers: colors.user_msg.modifiers,
            };
            pos_x = text::draw_chars(tb, pos_x, pos_y, style, self.value.chars());
            text::draw_chars(tb, pos_x, pos_y, colors.faded, NICKNAME_SUFFIX.chars());
        }
    }
}

impl InputArea {
    pub(crate) fn new(width: i32, max_lines: i32) -> InputArea {
        InputArea {
            buffer: InputLine::new(),
            cursor: 0,
            width,
            height: Some(1),
            max_lines,
            scroll: None,
            history: Vec::with_capacity(HIST_SIZE),
            mode: Mode::Edit,
            nick: None,
        }
    }

    pub(crate) fn set_nick(&mut self, value: String, color: usize) {
        self.nick = Some(Nickname::new(value, color))
    }

    pub(crate) fn get_nick(&self) -> Option<String> {
        self.nick.as_ref().map(|nick| nick.value.clone())
    }

    /// Resizes input area
    pub(crate) fn resize(&mut self, width: i32, max_lines: i32) {
        self.width = width;
        self.height = None;
        self.max_lines = max_lines;
        if self.scroll.is_some() {
            self.move_cursor(self.cursor);
        }
    }

    pub(crate) fn draw(
        &mut self,
        tb: &mut Buffer,
        colors: &Colors,
        pos_x: i32,
        parent_y: i32,
        parent_height: i32,
        msg_area: &mut MsgArea,
    ) -> Option<(u16, u16)> {
        let input_field_height = self.get_height(self.width).min(parent_height.max(0));
        if input_field_height == 0 {
            msg_area.resize(self.width, parent_height.max(0));
            return None;
        }
        // if the height of msg_area needs to change...
        if parent_height - input_field_height != msg_area.get_height() {
            msg_area.resize(self.width, parent_height - input_field_height);
        }
        let pos_y = parent_y + parent_height - input_field_height;
        let mut nick_length = 0;
        if let Some(nick) = &self.nick {
            nick.draw(tb, colors, pos_x, pos_y, self.width);
            nick_length = nick.len(self.width) as i32;
        }
        match self.mode {
            Mode::Edit => draw_line(
                tb,
                colors,
                &self.buffer,
                pos_x,
                pos_x + nick_length,
                pos_y,
                self.width,
                self.cursor,
                self.should_scroll(),
                self.scroll,
                None,
            ),
            Mode::History(hist_curs) => draw_line(
                tb,
                colors,
                &self.history[hist_curs as usize],
                pos_x,
                pos_x + nick_length,
                pos_y,
                self.width,
                self.cursor,
                self.should_scroll(),
                self.scroll,
                None,
            ),
            Mode::Autocomplete {
                ref original_buffer,
                insertion_point,
                word_starts,
                ref completions,
                current_completion,
            } => draw_line_autocomplete(
                original_buffer,
                insertion_point,
                word_starts,
                completions,
                current_completion,
                tb,
                colors,
                pos_x,
                pos_x + nick_length,
                pos_y,
                self.width,
                self.cursor,
                self.should_scroll(),
                self.scroll,
            ),
        }
    }

    pub(crate) fn keypressed(&mut self, key_action: &KeyAction) -> WidgetRet {
        match key_action {
            KeyAction::InputSend => {
                if self.current_buffer_len() > 0 {
                    self.modify();

                    let ret = mem::replace(&mut self.buffer, InputLine::new());
                    if self.history.len() == HIST_SIZE {
                        self.history.remove(0);
                    }
                    self.history.push(ret.clone());

                    self.move_cursor(0);

                    WidgetRet::Input(ret.get_buffer().to_owned())
                } else {
                    WidgetRet::KeyHandled
                }
            }
            KeyAction::InputDeletePrevChar => {
                if self.cursor > 0 {
                    self.modify();
                    let end = self.cursor as usize;
                    let start = prev_boundary(self.buffer.get_buffer(), end);
                    self.buffer.drain(start..end);
                    self.move_cursor(start as i32);
                }
                WidgetRet::KeyHandled
            }
            KeyAction::InputDeleteNextChar => {
                if self.cursor < self.current_buffer_len() {
                    self.modify();
                    let start = self.cursor as usize;
                    let end = next_boundary(self.buffer.get_buffer(), start);
                    self.buffer.drain(start..end);
                    self.move_cursor(self.cursor);
                }
                WidgetRet::KeyHandled
            }
            KeyAction::InputMoveCursStart => {
                self.move_cursor(0);
                WidgetRet::KeyHandled
            }
            KeyAction::InputMoveCursEnd => {
                self.move_cursor_to_end();
                WidgetRet::KeyHandled
            }
            KeyAction::InputDeleteToStart => {
                if self.cursor != 0 {
                    self.modify();
                    self.buffer.drain(..self.cursor as usize);
                    self.move_cursor(0);
                }
                WidgetRet::KeyHandled
            }
            KeyAction::InputDeleteToEnd => {
                if self.cursor != self.current_buffer_len() {
                    self.modify();
                    self.buffer.drain(self.cursor as usize..);
                    self.move_cursor(self.cursor);
                }
                WidgetRet::KeyHandled
            }
            KeyAction::InputDeletePrevWord => {
                self.consume_word_before_curs();
                WidgetRet::KeyHandled
            }
            KeyAction::InputMoveCursLeft => {
                self.dec_cursor();
                WidgetRet::KeyHandled
            }
            KeyAction::InputMoveCursRight => {
                self.inc_cursor();
                WidgetRet::KeyHandled
            }
            KeyAction::InputMoveWordLeft => {
                let text = self.current_text();
                let mut cur = self.cursor as usize;
                let mut skipped = false;
                while cur > 0 && grapheme_before(&text, cur).1.is_whitespace() {
                    cur = grapheme_before(&text, cur).0;
                    skipped = true;
                }
                while cur > 0 && grapheme_before(&text, cur).1.is_alphanumeric() {
                    cur = grapheme_before(&text, cur).0;
                    skipped = true;
                }
                if !skipped && cur > 0 {
                    cur = prev_boundary(&text, cur);
                }
                drop(text);
                self.move_cursor(cur as i32);
                WidgetRet::KeyHandled
            }
            KeyAction::InputMoveWordRight => {
                let text = self.current_text();
                let mut cur = self.cursor as usize;
                let mut skipped = false;
                while cur < text.len() && text[cur..].chars().next().unwrap().is_alphanumeric() {
                    cur = next_boundary(&text, cur);
                    skipped = true;
                }
                while cur < text.len() && text[cur..].chars().next().unwrap().is_whitespace() {
                    cur = next_boundary(&text, cur);
                    skipped = true;
                }
                if !skipped && cur < text.len() {
                    cur = next_boundary(&text, cur);
                }
                drop(text);
                self.move_cursor(cur as i32);
                WidgetRet::KeyHandled
            }
            KeyAction::InputPrevEntry => {
                self.completion_prev_entry();
                WidgetRet::KeyHandled
            }
            KeyAction::InputNextEntry => {
                self.completion_next_entry();
                WidgetRet::KeyHandled
            }
            KeyAction::Input(ch) => {
                self.modify();
                let pos = self.cursor as usize;
                self.buffer.insert(pos, *ch);
                let end = pos + ch.len_utf8();
                let text = self.buffer.get_buffer();
                let cursor = text
                    .grapheme_indices(true)
                    .map(|(i, _)| i)
                    .chain(std::iter::once(text.len()))
                    .find(|&i| i >= end)
                    .unwrap();
                self.move_cursor(cursor as i32);
                WidgetRet::KeyHandled
            }
            KeyAction::Command(cmd) => WidgetRet::Command(cmd.to_owned()),
            _ => WidgetRet::KeyIgnored,
        }
    }

    /// Get contents of the text field and cursor location and clear it.
    pub(crate) fn flush(&mut self) -> (String, usize) {
        self.modify();
        let cursor = mem::replace(&mut self.cursor, 0);
        let contents = self.buffer.drain(..).collect();
        self.move_cursor(0);
        (contents, cursor as usize)
    }

    /// Add a line to the text field history.
    pub(crate) fn add_history(&mut self, str: &str) {
        self.history.push(InputLine::from_buffer(str.to_owned()));
    }

    pub(crate) fn set(&mut self, str: &str) {
        self.mode = Mode::Edit;
        self.buffer = InputLine::from_buffer(str.to_owned());
        self.height = None;
        self.move_cursor_to_end();
    }

    pub(crate) fn set_cursor(&mut self, cursor: usize) {
        let text = self.current_text();
        let cursor = text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .take_while(|&i| i <= cursor)
            .last()
            .unwrap_or(0);
        drop(text);
        self.move_cursor(cursor as i32);
    }

    fn consume_word_before_curs(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.modify();
        let text = self.buffer.get_buffer();
        let end = self.cursor as usize;
        let mut start = end;
        let (_, ch) = grapheme_before(text, start);
        if ch.is_whitespace() {
            while start > 0 && grapheme_before(text, start).1.is_whitespace() {
                start = prev_boundary(text, start);
            }
            while start > 0 && grapheme_before(text, start).1.is_alphanumeric() {
                start = prev_boundary(text, start);
            }
        } else if ch.is_alphanumeric() {
            while start > 0 && grapheme_before(text, start).1.is_alphanumeric() {
                start = prev_boundary(text, start);
            }
        } else {
            start = prev_boundary(text, start);
        }
        self.buffer.drain(start..end);
        self.move_cursor(start as i32);
    }

    fn completion_prev_entry(&mut self) {
        // invalidate height calculation
        self.height = None;
        let mode = mem::replace(&mut self.mode, Mode::Edit);

        match mode {
            Mode::Edit => {
                if !self.history.is_empty() {
                    self.mode = Mode::History((self.history.len() as i32) - 1);
                    self.move_cursor_to_end();
                }
            }
            Mode::History(hist_curs) => {
                self.mode = Mode::History(if hist_curs > 0 {
                    hist_curs - 1
                } else {
                    hist_curs
                });
                self.move_cursor_to_end();
            }
            Mode::Autocomplete {
                original_buffer,
                insertion_point,
                word_starts,
                completions,
                current_completion,
                ..
            } => {
                let current_completion = if current_completion == completions.len() - 1 {
                    0
                } else {
                    current_completion + 1
                };

                let cursor = (insertion_point + completions[current_completion].len()) as i32;

                self.mode = Mode::Autocomplete {
                    original_buffer,
                    insertion_point,
                    word_starts,
                    completions,
                    current_completion,
                };

                self.move_cursor(cursor);
            }
        }
    }

    fn completion_next_entry(&mut self) {
        // invalidate height calculation
        self.height = None;
        let mode = mem::replace(&mut self.mode, Mode::Edit);

        match mode {
            Mode::Edit => {}
            Mode::History(hist_curs) => {
                if hist_curs != (self.history.len() - 1) as i32 {
                    self.mode = Mode::History(hist_curs + 1);
                } else {
                    self.mode = Mode::Edit;
                }
                self.move_cursor_to_end();
            }
            Mode::Autocomplete {
                original_buffer,
                insertion_point,
                word_starts,
                completions,
                current_completion,
                ..
            } => {
                let current_completion = if current_completion == 0 {
                    completions.len() - 1
                } else {
                    current_completion - 1
                };

                let cursor = (insertion_point + completions[current_completion].len()) as i32;

                self.mode = Mode::Autocomplete {
                    original_buffer,
                    insertion_point,
                    word_starts,
                    completions,
                    current_completion,
                };

                self.move_cursor(cursor);
            }
        }
    }

    // Ignoring auto-completions
    pub(crate) fn shown_line(&mut self) -> &mut InputLine {
        match &mut self.mode {
            Mode::Edit | Mode::Autocomplete { .. } => &mut self.buffer,
            Mode::History(hist_curs) => &mut self.history[*hist_curs as usize],
        }
    }

    fn calculate_height_autocomplete(&self, width: i32, nick_length: usize) -> usize {
        if self.in_autocomplete() {
            let mut layout = LineDataCache::input_line(width, nick_length);
            layout.calculate_height(self.current_text().chars(), 0);
            layout.get_line_count().unwrap()
        } else {
            1
        }
    }

    /// Gets the height of the widget. If the height is larger than the allowed max lines (set on
    /// initialization) turn on scroll else turn off scroll.
    pub(crate) fn get_height(&mut self, width: i32) -> i32 {
        let height = match self.height {
            Some(height) => height,
            None => self.calculate_height(width),
        };

        // Check for scroll fallback
        if height >= self.max_lines || width <= SCROLL_FALLBACK_WIDTH {
            self.scroll_on();
            1
        } else {
            self.scroll_off();
            height
        }
    }

    fn scroll_on(&mut self) {
        if self.scroll.is_none() {
            self.scroll = Some(0);
        }
        self.move_cursor(self.cursor);
    }

    fn scroll_off(&mut self) {
        self.scroll = None;
    }

    fn calculate_height(&mut self, width: i32) -> i32 {
        let mut nick_length = 0;
        if let Some(nick) = &self.nick {
            nick_length = nick.len(self.width);
        }
        let line_count = if self.in_autocomplete() {
            self.calculate_height_autocomplete(width, nick_length) as i32
        } else {
            self.shown_line().calculate_height(width, nick_length) as i32
        };
        self.height = Some(line_count);
        line_count
    }

    fn current_buffer_len(&self) -> i32 {
        match self.mode {
            Mode::Edit => self.buffer.len() as i32,
            Mode::History(hist_curs) => self.history[hist_curs as usize].len() as i32,
            Mode::Autocomplete {
                ref original_buffer,
                ref completions,
                current_completion,
                ..
            } => (original_buffer.len() + completions[current_completion].len()) as i32,
        }
    }

    fn current_text(&self) -> std::borrow::Cow<'_, str> {
        match &self.mode {
            Mode::Edit => self.buffer.get_buffer().into(),
            Mode::History(index) => self.history[*index as usize].get_buffer().into(),
            Mode::Autocomplete {
                original_buffer,
                insertion_point,
                completions,
                current_completion,
                ..
            } => {
                let mut text = original_buffer.get_buffer().to_owned();
                text.insert_str(*insertion_point, &completions[*current_completion]);
                text.into()
            }
        }
    }

    ////////////////////////////////////////////////////////////////////////////

    fn in_autocomplete(&self) -> bool {
        matches!(self.mode, Mode::Autocomplete { .. })
    }

    fn modify(&mut self) {
        // invalidate height calculation
        self.height = None;
        match self.mode {
            Mode::Edit => {}
            Mode::History(hist_idx) => {
                self.buffer = self.history[hist_idx as usize].clone();
            }
            Mode::Autocomplete {
                ref mut original_buffer,
                insertion_point,
                ref mut completions,
                current_completion,
                ..
            } => {
                let mut buffer = mem::replace(original_buffer, InputLine::new());
                let completions: Vec<String> = mem::take(completions);
                let word = &completions[current_completion];

                buffer.insert_str(insertion_point, word);

                self.buffer = buffer
            }
        }
        self.mode = Mode::Edit;
    }

    ////////////////////////////////////////////////////////////////////////////
    // Manipulating cursor

    fn inc_cursor(&mut self) {
        let text = self.current_text();
        let cursor = next_boundary(&text, self.cursor as usize);
        drop(text);
        self.move_cursor(cursor as i32);
    }
    fn dec_cursor(&mut self) {
        let text = self.current_text();
        let cursor = prev_boundary(&text, self.cursor as usize);
        drop(text);
        self.move_cursor(cursor as i32);
    }
    fn move_cursor_to_end(&mut self) {
        self.move_cursor(self.current_buffer_len());
    }
    fn move_cursor(&mut self, cursor: i32) {
        let text = self.current_text();
        let requested = cursor.clamp(0, text.len() as i32) as usize;
        let cursor = text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .find(|&i| i >= requested)
            .unwrap();
        let new_scroll = if let Some(scroll) = self.scroll {
            let nick_length = self.nick.as_ref().map_or(0, |nick| nick.len(self.width));
            let width = (self.width - nick_length as i32).max(1);
            let mut column = 0;
            let mut start = (0, 0);
            let mut cursor_col = 0;
            let mut cursor_width = 1;
            for (idx, grapheme) in text.grapheme_indices(true) {
                let grapheme_width = (UnicodeWidthStr::width(grapheme).max(1) as i32).min(width);
                if idx <= scroll.max(0) as usize {
                    start = (idx, column);
                }
                if idx == cursor {
                    cursor_col = column;
                    cursor_width = grapheme_width;
                }
                column += grapheme_width;
            }
            if scroll.max(0) as usize >= text.len() {
                start = (text.len(), column);
            }
            if cursor == text.len() {
                cursor_col = column;
            }
            let margin = if width < 2 * SCROLL_OFF + 1 {
                0
            } else {
                SCROLL_OFF
            };
            let target = if column + 1 < width {
                0
            } else if cursor_col - margin < start.1 {
                (cursor_col - margin).max(0)
            } else if cursor_col + cursor_width - 1 + margin >= (start.1 + width).min(column) {
                (cursor_col + cursor_width + margin - width)
                    .max(0)
                    .min((column + 1 - width).max(0))
            } else {
                start.1
            };
            if target != start.1 {
                let mut col = 0;
                let mut chosen = None;
                let mut last_boundary = 0;
                for (idx, grapheme) in text
                    .grapheme_indices(true)
                    .chain(std::iter::once((text.len(), "")))
                {
                    if idx > cursor {
                        break;
                    }
                    last_boundary = idx;
                    if target < start.1 && col <= target {
                        chosen = Some(idx);
                    } else if target > start.1 && idx >= start.0 && col >= target {
                        chosen = Some(idx);
                        break;
                    }
                    col += (UnicodeWidthStr::width(grapheme).max(1) as i32).min(width);
                }
                start.0 = chosen.unwrap_or(if target < start.1 { 0 } else { last_boundary });
            }
            Some(start.0 as i32)
        } else {
            None
        };
        drop(text);
        self.scroll = new_scroll;
        self.cursor = cursor as i32;
    }
}

impl InputArea {
    pub(crate) fn autocomplete(&mut self, nicks: &Trie, command_completions: &Trie) {
        if self.in_autocomplete() {
            // scroll next if you hit the KeyAction::InputAutoComplete key again
            self.completion_prev_entry();
            return;
        }

        // invalidate height calculation
        self.height = None;

        let cursor_right = self.cursor;
        let (completions, word_starts) = {
            let line = self.shown_line().get_buffer();
            let cursor = cursor_right as usize;
            let command_end = if line.starts_with('/') && cursor > 0 {
                let end = line
                    .char_indices()
                    .find(|(_, c)| c.is_whitespace())
                    .map_or(line.len(), |(i, _)| i);
                (cursor <= end).then_some(end)
            } else {
                None
            };
            if let Some(end) = command_end {
                let suffix = &line[cursor..end];
                let completions = command_completions
                    .drop_pfx(&mut line[1..cursor].chars())
                    .into_iter()
                    .filter_map(|mut completion| {
                        if completion.ends_with(suffix) {
                            completion.truncate(completion.len() - suffix.len());
                            Some(completion)
                        } else {
                            None
                        }
                    })
                    .collect();
                (completions, 1)
            } else {
                let mut start = cursor;
                while start > 0 {
                    let prev = prev_boundary(line, start);
                    if !line[prev..start]
                        .chars()
                        .next()
                        .is_some_and(utils::is_nick_char)
                    {
                        break;
                    }
                    start = prev;
                }
                (nicks.drop_pfx(&mut line[start..cursor].chars()), start)
            }
        };

        if !completions.is_empty() {
            let completion_len = completions[0].len();
            self.mode = Mode::Autocomplete {
                original_buffer: self.shown_line().to_owned(),
                insertion_point: self.cursor as usize,
                word_starts,
                completions,
                current_completion: 0,
            };
            let cursor = self.cursor;
            self.move_cursor(cursor + completion_len as i32);
        }
    }

    fn should_scroll(&self) -> bool {
        self.scroll.is_some()
    }
}

fn prev_boundary(text: &str, pos: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .take_while(|&i| i < pos)
        .last()
        .unwrap_or(0)
}
fn next_boundary(text: &str, pos: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .find(|&i| i > pos)
        .unwrap_or(text.len())
}
fn grapheme_before(text: &str, pos: usize) -> (usize, char) {
    let start = prev_boundary(text, pos);
    (start, text[start..].chars().next().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_editing_history_completion_and_byte_cursor() {
        let mut input = InputArea::new(80, 4);
        input.set("Aか\u{3099}👩‍💻B");
        input.keypressed(&KeyAction::InputMoveCursLeft);
        input.keypressed(&KeyAction::InputDeletePrevChar);
        assert_eq!(input.current_text(), "Aか\u{3099}B");
        input.keypressed(&KeyAction::InputDeleteNextChar);
        assert_eq!(input.current_text(), "Aか\u{3099}");
        let WidgetRet::Input(sent) = input.keypressed(&KeyAction::InputSend) else {
            panic!("expected input")
        };
        assert_eq!(sent, "Aか\u{3099}");
        input.keypressed(&KeyAction::InputPrevEntry);
        assert_eq!(input.current_text(), sent);
        input.keypressed(&KeyAction::InputMoveCursLeft);
        let (saved, cursor) = input.flush();
        assert_eq!(saved, sent);
        assert_eq!(cursor, 1);
        input.set(&saved);
        input.set_cursor(cursor);
        input.keypressed(&KeyAction::Input('X'));
        assert_eq!(input.current_text(), "AXか\u{3099}");
        input.set("/hel");
        let mut commands = Trie::new();
        commands.insert("help");
        input.autocomplete(&Trie::new(), &commands);
        assert_eq!(input.current_text(), "/help");
        input.keypressed(&KeyAction::Input('!'));
        assert_eq!(input.current_text(), "/help!");
        input.set("AB");
        input.set_cursor(1);
        input.keypressed(&KeyAction::Input('\u{301}'));
        assert_eq!(input.flush(), ("A\u{301}B".to_owned(), 3));
    }
    #[test]
    fn completion_preserves_unicode_suffix_and_history() {
        let mut commands = Trie::new();
        commands.insert("join");
        let mut input = InputArea::new(80, 10);
        input.add_history("/jo\u{2003}café λ");
        input.keypressed(&KeyAction::InputPrevEntry);
        input.set_cursor(3);
        input.autocomplete(&Trie::new(), &commands);
        input.keypressed(&KeyAction::Input('!'));
        assert_eq!(input.current_text(), "/join!\u{2003}café λ");
        input.keypressed(&KeyAction::InputSend);
        input.keypressed(&KeyAction::InputPrevEntry);
        input.keypressed(&KeyAction::InputPrevEntry);
        assert_eq!(input.current_text(), "/jo\u{2003}café λ");
        input.set("/jn 東京");
        input.set_cursor(2);
        input.autocomplete(&Trie::new(), &commands);
        assert_eq!(input.current_text(), "/join 東京");
    }

    #[test]
    fn unicode_completion_cycles_and_preserves_graphemes() {
        let mut dict = Trie::new();
        dict.insert("日本語");
        dict.insert("日光");
        let mut input = InputArea::new(8, 50);
        input.set("日!끝");
        input.set_cursor("日".len());
        input.autocomplete(&dict, &Trie::new());
        assert_eq!(input.current_text(), "日光!끝");
        input.keypressed(&KeyAction::InputPrevEntry);
        assert_eq!(input.current_text(), "日本語!끝");
        input.keypressed(&KeyAction::InputNextEntry);
        input.keypressed(&KeyAction::Input('中'));
        assert_eq!(input.current_text(), "日光中!끝");
        let mut dict = Trie::new();
        dict.insert("か\u{3099}め");
        input.set("say か\u{3099} !");
        input.set_cursor("say か\u{3099}".len());
        input.autocomplete(&dict, &Trie::new());
        assert_eq!(input.current_text(), "say か\u{3099}め !");
    }

    #[test]
    fn word_motion_and_input_height() {
        let mut input = InputArea::new(40, 50);
        input.set("a b c");
        input.keypressed(&KeyAction::InputSend);
        input.keypressed(&KeyAction::InputPrevEntry);
        input.keypressed(&KeyAction::InputMoveWordLeft);
        assert_eq!(input.cursor, 4);
        input.keypressed(&KeyAction::InputMoveWordRight);
        assert_eq!(input.cursor, 5);
        input.set(&"a".repeat(41));
        assert_eq!(input.get_height(40), 2);
    }

    #[test]
    fn nickname_and_cursor_render_in_buffer() {
        use crate::msg_area::Layout;
        use ratatui::layout::Rect;
        let nick = Nickname::new("\u{3099}日".into(), 0);
        assert_eq!(nick.len(17), 5);
        let mut input = InputArea::new(20, 4);
        input.set_nick("日".into(), 0);
        input.set("か\u{3099}B");
        input.set_cursor(0);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 3));
        let colors = Colors::default();
        let mut messages = MsgArea::new(20, 2, 10, Layout::Compact);
        assert_eq!(
            input.draw(&mut buffer, &colors, 0, 0, 3, &mut messages),
            Some((4, 2))
        );
        assert_eq!(buffer[(0, 2)].symbol(), "日");
        assert_eq!(buffer[(2, 2)].symbol(), ":");
        assert_eq!(buffer[(4, 2)].symbol(), "か\u{3099}");
        assert_eq!(buffer[(4, 2)].fg, colors.cursor.fg);
    }
}
