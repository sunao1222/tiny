use crate::{config::Colors, line_split::LineDataCache, text};
use ratatui::buffer::Buffer;
use std::ops::RangeBounds;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug)]
pub(crate) struct InputLine {
    buffer: String,
    line_data: LineDataCache,
}

impl InputLine {
    pub(crate) fn new() -> Self {
        Self::from_buffer(String::new())
    }
    pub(crate) fn from_buffer(buffer: String) -> Self {
        Self {
            buffer,
            line_data: LineDataCache::input_line(0, 0),
        }
    }
    pub(crate) fn get_buffer(&self) -> &str {
        &self.buffer
    }
    pub(crate) fn len(&self) -> usize {
        self.buffer.len()
    }
    pub(crate) fn drain<R: RangeBounds<usize>>(&mut self, range: R) -> std::string::Drain<'_> {
        self.line_data.set_dirty();
        self.buffer.drain(range)
    }
    pub(crate) fn insert(&mut self, idx: usize, ch: char) {
        self.buffer.insert(idx, ch);
        self.line_data.set_dirty();
    }
    pub(crate) fn insert_str(&mut self, idx: usize, text: &str) {
        self.buffer.insert_str(idx, text);
        self.line_data.set_dirty();
    }
    pub(crate) fn calculate_height(&mut self, width: i32, nick_length: usize) -> usize {
        if self.line_data.is_dirty() || self.line_data.needs_resize(width, nick_length, None) {
            self.line_data = LineDataCache::input_line(width, nick_length);
            self.line_data.calculate_height(self.buffer.chars(), 0);
        }
        self.line_data.get_line_count().unwrap()
    }
}

pub(crate) struct CompletionRange {
    start_idx: usize,
    end_idx: usize,
}

fn draw_input(
    buffer: &mut Buffer,
    colors: &Colors,
    line: &str,
    left: i32,
    pos_x: i32,
    pos_y: i32,
    width: i32,
    cursor: i32,
    scroll: Option<i32>,
    splits: &[i32],
    completion: Option<CompletionRange>,
) -> Option<(u16, u16)> {
    let area = buffer.area;
    let right = (left + width).min(area.right() as i32);
    if right <= area.x as i32 || pos_y < area.y as i32 || pos_y >= area.bottom() as i32 {
        return None;
    }
    let start = scroll.unwrap_or(0).max(0) as usize;
    let cursor = cursor.max(0) as usize;
    let mut x = pos_x;
    let mut y = pos_y;
    let mut scalar = 0;
    let mut split_iter = splits.iter().copied().peekable();
    let mut cursor_xy = None;
    let mut complete = true;
    for (idx, grapheme) in line.grapheme_indices(true) {
        let scalar_start = scalar;
        scalar += grapheme.chars().count() as i32;
        if idx < start {
            continue;
        }
        if scroll.is_none() {
            while split_iter
                .peek()
                .is_some_and(|&split| split == scalar_start)
            {
                split_iter.next();
                x = left;
                y += 1;
            }
        }
        if y >= area.bottom() as i32 || x >= right {
            complete = false;
            break;
        }
        if idx == cursor {
            cursor_xy = Some((x, y));
        }
        let style = if completion
            .as_ref()
            .is_some_and(|r| idx >= r.start_idx && idx < r.end_idx)
        {
            colors.completion
        } else {
            colors.user_msg
        };
        let columns = UnicodeWidthStr::width(grapheme).max(1) as i32;
        if columns > right - x {
            if right - x == 1 {
                text::draw_grapheme(buffer, x, y, style, grapheme);
            }
            complete = false;
            break;
        }
        text::draw_grapheme(buffer, x, y, style, grapheme);
        x += columns;
    }
    if cursor == line.len() && complete {
        if scroll.is_none() && x >= right {
            x = left;
            y += 1;
        }
        cursor_xy = Some((x.min(right - 1), y));
    }
    let (cx, cy) = cursor_xy?;
    if cx < area.x as i32
        || cx >= area.right() as i32
        || cy < area.y as i32
        || cy >= area.bottom() as i32
    {
        return None;
    }
    if let Some(cell) = buffer.cell_mut((cx as u16, cy as u16)) {
        cell.set_style(ratatui::style::Style::from(colors.cursor));
    }
    Some((cx as u16, cy as u16))
}

pub(crate) fn draw_line_autocomplete(
    original_buffer: &InputLine,
    insertion_point: usize,
    word_starts: usize,
    completions: &[String],
    current_completion: usize,
    buffer: &mut Buffer,
    colors: &Colors,
    left: i32,
    pos_x: i32,
    pos_y: i32,
    width: i32,
    cursor: i32,
    should_scroll: bool,
    scroll: Option<i32>,
) -> Option<(u16, u16)> {
    let completion = &completions[current_completion];
    let mut line = original_buffer.buffer.clone();
    line.insert_str(insertion_point, completion);
    let mut layout = LineDataCache::input_line(width, (pos_x - left).max(0) as usize);
    if !should_scroll {
        layout.calculate_height(line.chars(), 0);
    }
    draw_input(
        buffer,
        colors,
        &line,
        left,
        pos_x,
        pos_y,
        width,
        cursor,
        if should_scroll {
            Some(scroll.unwrap_or(0))
        } else {
            None
        },
        layout.get_splits(),
        Some(CompletionRange {
            start_idx: word_starts,
            end_idx: insertion_point + completion.len(),
        }),
    )
}

pub(crate) fn draw_line(
    buffer: &mut Buffer,
    colors: &Colors,
    line: &InputLine,
    left: i32,
    pos_x: i32,
    pos_y: i32,
    width: i32,
    cursor: i32,
    should_scroll: bool,
    scroll: Option<i32>,
    completion: Option<CompletionRange>,
) -> Option<(u16, u16)> {
    draw_input(
        buffer,
        colors,
        &line.buffer,
        left,
        pos_x,
        pos_y,
        width,
        cursor,
        if should_scroll {
            Some(scroll.unwrap_or(0))
        } else {
            None
        },
        line.line_data.get_splits(),
        completion,
    )
}
