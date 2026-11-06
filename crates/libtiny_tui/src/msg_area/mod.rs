pub(crate) mod line;

use ratatui::buffer::Buffer;
use std::cmp::max;
use std::collections::VecDeque;

use self::line::{Line, SegStyle};
use crate::config::Colors;
use crate::line_split::{LineDataCache, LineType};
use crate::messaging::{MSG_NICK_SUFFIX_LEN, Timestamp, aligned_nick, nick_color};

#[derive(Debug)]
pub(crate) enum ActivityEvent {
    Join(String),
    Part(String),
    Nick { old: String, new: String },
}

#[derive(Debug)]
pub(crate) enum HistoryEntry {
    Client {
        text: String,
        style: SegStyle,
    },
    Topic {
        timestamp: Timestamp,
        show_timestamp: bool,
        text: String,
    },
    Message {
        timestamp: Timestamp,
        show_timestamp: bool,
        text: String,
        is_error: bool,
    },
    Privmsg {
        timestamp: Timestamp,
        show_timestamp: bool,
        sender: String,
        text: String,
        highlight: bool,
        is_action: bool,
    },
    Activity {
        timestamp: Timestamp,
        show_timestamp: bool,
        events: Vec<ActivityEvent>,
    },
}

struct HistoryLine {
    entry: HistoryEntry,
    cache: LineDataCache,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Layout {
    Compact,
    Aligned { max_nick_len: usize },
}

impl Layout {
    pub(crate) fn is_aligned(&self) -> bool {
        matches!(self, Layout::Aligned { .. })
    }

    fn msg_padding(&self) -> usize {
        match self {
            Layout::Compact => 0,
            Layout::Aligned { max_nick_len } => {
                Timestamp::WIDTH + max_nick_len + MSG_NICK_SUFFIX_LEN
            }
        }
    }
}

// Build only temporary display spans. Strings belonging to history are borrowed;
// generated timestamp, padding and nick columns live only during this call.
fn with_line<R>(
    entry: &HistoryEntry,
    layout: Layout,
    cache: &mut LineDataCache,
    f: impl FnOnce(&mut Line<'_>) -> R,
) -> R {
    let (timestamp, show_timestamp) = match entry {
        HistoryEntry::Client { .. } => (None, false),
        HistoryEntry::Topic {
            timestamp,
            show_timestamp,
            ..
        }
        | HistoryEntry::Message {
            timestamp,
            show_timestamp,
            ..
        }
        | HistoryEntry::Privmsg {
            timestamp,
            show_timestamp,
            ..
        }
        | HistoryEntry::Activity {
            timestamp,
            show_timestamp,
            ..
        } => (Some(*timestamp), *show_timestamp),
    };
    let stamp = timestamp.map(|ts| ts.stamp()).unwrap_or_default();
    let nick_column = match (entry, layout) {
        (
            HistoryEntry::Privmsg {
                sender, is_action, ..
            },
            Layout::Aligned { max_nick_len },
        ) => aligned_nick(if *is_action { "**" } else { sender }, max_nick_len),
        _ => String::new(),
    };
    let padding = match (entry, layout) {
        (HistoryEntry::Activity { .. }, Layout::Aligned { max_nick_len }) => {
            " ".repeat(max_nick_len + MSG_NICK_SUFFIX_LEN)
        }
        _ => String::new(),
    };
    let mut line = Line::new();
    if timestamp.is_some() {
        if show_timestamp {
            line.add_text(&stamp, SegStyle::Timestamp);
        } else if layout.is_aligned() {
            line.add_text(Timestamp::BLANK, SegStyle::Timestamp);
        }
    }
    match entry {
        HistoryEntry::Client { text, style } => line.add_text(text, *style),
        HistoryEntry::Topic { text, .. } => line.add_independent_text(text, SegStyle::Topic),
        HistoryEntry::Message { text, is_error, .. } => line.add_independent_text(
            text,
            if *is_error {
                SegStyle::ErrMsg
            } else {
                SegStyle::UserMsg
            },
        ),
        HistoryEntry::Privmsg {
            sender,
            text,
            highlight,
            is_action,
            ..
        } => {
            let nick = if layout.is_aligned() {
                nick_column.as_str()
            } else if *is_action {
                "**"
            } else {
                sender.as_str()
            };
            let style = SegStyle::NickColor(nick_color(sender));
            if *is_action {
                line.add_text(nick, SegStyle::UserMsg);
                line.add_text("  ", SegStyle::Faded);
                line.add_independent_text(sender, style);
                line.add_text(" ", SegStyle::UserMsg);
            } else {
                line.add_independent_text(nick, style);
                line.add_text(": ", SegStyle::Faded);
            }
            line.add_independent_text(
                text,
                if *highlight {
                    SegStyle::Highlight
                } else {
                    SegStyle::UserMsg
                },
            );
            line.set_type(LineType::AlignedMsg {
                msg_padding: layout.msg_padding(),
            });
        }
        HistoryEntry::Activity { events, .. } => {
            line.add_text(&padding, SegStyle::UserMsg);
            line.set_type(LineType::AlignedMsg {
                msg_padding: layout.msg_padding(),
            });
            for (i, event) in events.iter().enumerate() {
                if i != 0 {
                    line.add_text(" ", SegStyle::UserMsg);
                }
                match event {
                    ActivityEvent::Join(nick) => {
                        line.add_text("+", SegStyle::Join);
                        line.add_text(nick, SegStyle::Faded);
                    }
                    ActivityEvent::Part(nick) => {
                        line.add_text("-", SegStyle::Part);
                        line.add_text(nick, SegStyle::Faded);
                    }
                    ActivityEvent::Nick { old, new } => {
                        line.add_text(old, SegStyle::Faded);
                        line.add_text(">", SegStyle::Nick);
                        line.add_text(new, SegStyle::Faded);
                    }
                }
            }
        }
    }
    let line_type = line.line_type();
    line.swap_cache(cache);
    line.set_type(line_type);
    let result = f(&mut line);
    line.swap_cache(cache);
    result
}

pub(crate) struct MsgArea {
    lines: VecDeque<HistoryLine>,
    scrollback: usize,
    width: i32,
    height: i32,
    scroll: i32,
    lines_height: Option<i32>,
    layout: Layout,
}

impl MsgArea {
    pub(crate) fn new(width: i32, height: i32, scrollback: usize, layout: Layout) -> MsgArea {
        let scrollback = scrollback.max(1);
        MsgArea {
            lines: VecDeque::with_capacity(512.min(scrollback)),
            scrollback,
            width,
            height,
            scroll: 0,
            lines_height: Some(0),
            layout,
        }
    }

    pub(crate) fn get_height(&self) -> i32 {
        self.height
    }

    fn line_height(&mut self, index: usize) -> i32 {
        let line = &mut self.lines[index];
        if let Some(height) = line.cache.get_line_count() {
            return height as i32;
        }
        with_line(&line.entry, self.layout, &mut line.cache, |line| {
            line.rendered_height(self.width)
        })
    }

    fn update_total_visible_lines(&mut self) -> i32 {
        if let Some(height) = self.lines_height {
            return height;
        }
        let height = (0..self.lines.len()).map(|i| self.line_height(i)).sum();
        self.lines_height = Some(height);
        height
    }

    // The anchor is a logical entry and the offset of the top screen row within it.
    // None means that the viewport follows the bottom.
    fn anchor(&mut self) -> Option<(usize, i32)> {
        if self.scroll == 0 || self.lines.is_empty() {
            return None;
        }
        let mut top = max(
            0,
            self.update_total_visible_lines() - self.scroll - max(0, self.height),
        );
        for i in 0..self.lines.len() {
            let height = self.line_height(i);
            if top < height {
                return Some((i, top));
            }
            top -= height;
        }
        Some((self.lines.len() - 1, 0))
    }

    fn restore_anchor(&mut self, anchor: Option<(usize, i32)>, removed: usize) {
        let Some((index, offset)) = anchor else {
            self.scroll = 0;
            return;
        };
        if self.lines.is_empty() {
            self.scroll = 0;
            return;
        }
        let removed_anchor = index < removed;
        let index = index.saturating_sub(removed).min(self.lines.len() - 1);
        let offset = if removed_anchor {
            0
        } else {
            offset.min(self.line_height(index) - 1)
        };
        let before: i32 = (0..index).map(|i| self.line_height(i)).sum();
        let total = self.update_total_visible_lines();
        self.scroll = max(0, total - max(0, self.height) - before - offset);
    }

    fn invalidate(&mut self) {
        self.lines_height = None;
        for line in &mut self.lines {
            line.cache.set_dirty();
        }
    }

    pub(crate) fn resize(&mut self, width: i32, height: i32) {
        let anchor = self.anchor();
        self.width = width;
        self.height = height;
        self.invalidate();
        self.restore_anchor(anchor, 0);
    }

    pub(crate) fn apply_config(&mut self, scrollback: usize, layout: Layout) {
        let anchor = self.anchor();
        self.scrollback = scrollback.max(1);
        self.layout = layout;
        let removed = self.lines.len().saturating_sub(self.scrollback);
        for _ in 0..removed {
            self.lines.pop_front();
        }
        self.invalidate();
        self.restore_anchor(anchor, removed);
    }

    pub(crate) fn push(&mut self, entry: HistoryEntry) {
        let anchor = self.anchor();
        self.lines.push_back(HistoryLine {
            entry,
            cache: LineDataCache::msg_line(0, None),
        });
        let removed = self.lines.len().saturating_sub(self.scrollback);
        for _ in 0..removed {
            self.lines.pop_front();
        }
        self.lines_height = None;
        self.restore_anchor(anchor, removed);
    }

    pub(crate) fn append_activity(
        &mut self,
        ts: Timestamp,
        event: ActivityEvent,
    ) -> Result<(), ActivityEvent> {
        if !matches!(self.lines.back().map(|line| &line.entry), Some(HistoryEntry::Activity { timestamp, .. }) if *timestamp == ts)
        {
            return Err(event);
        }
        let anchor = self.anchor();
        let line = self.lines.back_mut().unwrap();
        if let HistoryEntry::Activity { events, .. } = &mut line.entry {
            events.push(event);
        }
        line.cache.set_dirty();
        self.lines_height = None;
        self.restore_anchor(anchor, 0);
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.scroll = 0;
        self.lines_height = Some(0);
    }

    pub(crate) fn draw(&mut self, buffer: &mut Buffer, colors: &Colors, pos_x: i32, pos_y: i32) {
        let mut row = pos_y + self.height - 1;
        let mut skip = self.scroll;
        for index in (0..self.lines.len()).rev() {
            if row < pos_y {
                break;
            }
            let line_height = self.line_height(index);
            if skip >= line_height {
                skip -= line_height;
                continue;
            }
            let height = line_height - skip;
            let line_row = row - height + 1;
            let render_from = max(0, pos_y - line_row);
            let line = &mut self.lines[index];
            with_line(&line.entry, self.layout, &mut line.cache, |line| {
                line.draw(buffer, colors, pos_x, line_row, render_from, height);
            });
            row = line_row - 1;
            skip = 0;
            if line_row < pos_y {
                break;
            }
        }
    }

    pub(crate) fn scroll_up(&mut self) {
        if self.scroll < max(0, self.update_total_visible_lines() - self.height) {
            self.scroll += 1;
        }
    }
    pub(crate) fn scroll_down(&mut self) {
        if self.scroll > 0 {
            self.scroll -= 1;
        }
    }
    pub(crate) fn scroll_top(&mut self) {
        self.scroll = max(0, self.update_total_visible_lines() - self.height);
    }
    pub(crate) fn scroll_bottom(&mut self) {
        self.scroll = 0;
    }
    pub(crate) fn page_up(&mut self) {
        for _ in 0..10 {
            self.scroll_up();
        }
    }
    pub(crate) fn page_down(&mut self) {
        self.scroll = max(0, self.scroll - 10);
    }
}
