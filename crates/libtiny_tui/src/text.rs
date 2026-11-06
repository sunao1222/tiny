//! Grapheme-aware text measurement and drawing into Ratatui buffers.

use crate::config::Style;
use ratatui::buffer::Buffer;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display columns occupied by an independent piece of text.
pub(crate) fn text_width(text: &str) -> usize {
    text.graphemes(true)
        .map(|grapheme| UnicodeWidthStr::width(grapheme).max(1))
        .sum()
}

// Most clusters fit inline; only exceptionally long combining sequences need
// heap storage. This also lets clusters span independently styled IRC spans.
struct Cluster {
    inline: [u8; 64],
    len: usize,
    overflow: Option<String>,
}

impl Cluster {
    fn new() -> Self {
        Self {
            inline: [0; 64],
            len: 0,
            overflow: None,
        }
    }

    fn clear(&mut self) {
        self.len = 0;
        if let Some(text) = &mut self.overflow {
            text.clear();
        }
    }

    fn as_str(&self) -> &str {
        match &self.overflow {
            Some(text) => text,
            None => std::str::from_utf8(&self.inline[..self.len]).unwrap(),
        }
    }

    fn push(&mut self, ch: char) {
        if let Some(text) = &mut self.overflow {
            text.push(ch);
            return;
        }
        let mut bytes = [0; 4];
        let encoded = ch.encode_utf8(&mut bytes).as_bytes();
        if self.len + encoded.len() <= self.inline.len() {
            self.inline[self.len..self.len + encoded.len()].copy_from_slice(encoded);
            self.len += encoded.len();
        } else {
            let mut text = String::with_capacity(self.len + encoded.len());
            text.push_str(std::str::from_utf8(&self.inline[..self.len]).unwrap());
            text.push(ch);
            self.overflow = Some(text);
        }
    }

    fn truncate(&mut self, len: usize) {
        if let Some(text) = &mut self.overflow {
            text.truncate(len);
        } else {
            self.len = len;
        }
    }
}

/// Visit graphemes across span boundaries without allocating a copy of the text.
pub(crate) fn for_each_grapheme<T: Copy>(
    chars: impl Iterator<Item = (char, T)>,
    mut visit: impl FnMut(&str, T),
) {
    let mut chars = chars.peekable();
    let mut cluster = Cluster::new();
    while let Some((first, style)) = chars.next() {
        cluster.clear();
        cluster.push(first);
        while let Some((next, _)) = chars.peek().copied() {
            let previous_len = cluster.as_str().len();
            cluster.push(next);
            if cluster.as_str().graphemes(true).nth(1).is_some() {
                cluster.truncate(previous_len);
                break;
            }
            chars.next();
        }
        visit(cluster.as_str(), style);
    }
}

/// Draw one complete grapheme using the base character's style.
pub(crate) fn draw_grapheme(
    buffer: &mut Buffer,
    x: i32,
    y: i32,
    style: Style,
    grapheme: &str,
) -> i32 {
    let area = buffer.area;
    let left = i32::from(area.x);
    let right = left + i32::from(area.width);
    let top = i32::from(area.y);
    let bottom = top + i32::from(area.height);
    let width = UnicodeWidthStr::width(grapheme);
    let display_width = width.max(1) as i32;
    let clipped_wide = display_width == 2 && x == right - 1 && x >= left;
    if y >= top && y < bottom && x >= left && x < right {
        if let Some(cell) = buffer.cell_mut((x as u16, y as u16)) {
            cell.reset();
            if clipped_wide {
                let mut suffix = grapheme.chars();
                suffix.next();
                let suffix = suffix.as_str();
                if suffix.chars().all(|ch| {
                    ch != '\u{200d}' && unicode_width::UnicodeWidthChar::width(ch) == Some(0)
                }) {
                    let mut replacement = Cluster::new();
                    replacement.push('�');
                    for ch in suffix.chars() {
                        replacement.push(ch);
                    }
                    cell.set_symbol(replacement.as_str());
                } else {
                    cell.set_symbol("�");
                }
            } else if width == 0 {
                let mut dotted = Cluster::new();
                dotted.push('◌');
                for ch in grapheme.chars() {
                    dotted.push(ch);
                }
                cell.set_symbol(dotted.as_str());
            } else {
                cell.set_symbol(grapheme);
            }
            cell.set_style(style);
        }
        if display_width == 2 && !clipped_wide {
            if let Some(cell) = buffer.cell_mut(((x + 1) as u16, y as u16)) {
                cell.reset();
                cell.set_style(style);
            }
        }
    }
    x + if clipped_wide { 1 } else { display_width }
}

pub(crate) fn draw_styled_chars(
    buffer: &mut Buffer,
    mut x: i32,
    y: i32,
    chars: impl Iterator<Item = (char, Style)>,
) -> i32 {
    for_each_grapheme(chars, |grapheme, style| {
        x = draw_grapheme(buffer, x, y, style, grapheme);
    });
    x
}

pub(crate) fn draw_chars(
    buffer: &mut Buffer,
    x: i32,
    y: i32,
    style: Style,
    chars: impl Iterator<Item = char>,
) -> i32 {
    draw_styled_chars(buffer, x, y, chars.map(|ch| (ch, style)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        layout::Rect,
        style::{Color, Modifier},
    };

    #[test]
    fn graphemes_use_base_style_and_width() {
        let base = Style {
            fg: Color::Indexed(1),
            bg: Color::Reset,
            modifiers: Modifier::BOLD,
        };
        let mark = Style {
            fg: Color::Indexed(2),
            ..base
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 5, 1));
        assert_eq!(text_width("か\u{3099}👩‍💻"), 4);
        assert_eq!(
            draw_styled_chars(
                &mut buffer,
                0,
                0,
                [
                    ('か', base),
                    ('\u{3099}', mark),
                    ('👩', base),
                    ('\u{200d}', mark),
                    ('💻', mark)
                ]
                .into_iter()
            ),
            4
        );
        assert_eq!(buffer[(0, 0)].symbol(), "か\u{3099}");
        assert_eq!(buffer[(0, 0)].fg, base.fg);
        assert_eq!(buffer[(2, 0)].symbol(), "👩‍💻");
        assert_eq!(buffer[(2, 0)].fg, base.fg);
    }

    #[test]
    fn leading_marks_clipping_and_narrow_wide_characters() {
        let style = Style {
            fg: Color::Reset,
            bg: Color::Reset,
            modifiers: Modifier::empty(),
        };
        let mut buffer = Buffer::empty(Rect::new(2, 1, 2, 1));
        assert_eq!(
            draw_chars(&mut buffer, 2, 1, style, "\u{3099}日".chars()),
            4
        );
        assert_eq!(buffer[(2, 1)].symbol(), "◌\u{3099}");
        assert_eq!(buffer[(3, 1)].symbol(), "�");
        let mut narrow = Buffer::empty(Rect::new(0, 0, 1, 1));
        draw_grapheme(&mut narrow, 0, 0, style, "か\u{3099}");
        assert_eq!(narrow[(0, 0)].symbol(), "�\u{3099}");
        draw_chars(&mut buffer, -2, -1, style, "abc".chars());
        let mut empty = Buffer::empty(Rect::new(0, 0, 0, 0));
        draw_chars(&mut empty, 0, 0, style, "日".chars());
    }

    #[test]
    fn long_combining_cluster_remains_a_single_grapheme() {
        let style = Style {
            fg: Color::Reset,
            bg: Color::Reset,
            modifiers: Modifier::empty(),
        };
        let text = format!("a{}B", "\u{301}".repeat(80));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
        draw_chars(&mut buffer, 0, 0, style, text.chars());
        assert_eq!(buffer[(0, 0)].symbol(), &text[..text.len() - 1]);
        assert_eq!(buffer[(1, 0)].symbol(), "B");
    }
}
