//! Grapheme-aware text measurement and drawing into Ratatui buffers.

use crate::config::Style;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
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
    let clip = buffer.area;
    draw_grapheme_clipped(buffer, clip, x, y, style, grapheme)
}

fn draw_grapheme_clipped(
    buffer: &mut Buffer,
    clip: Rect,
    x: i32,
    y: i32,
    style: Style,
    grapheme: &str,
) -> i32 {
    let area = buffer.area;
    let left = i32::from(area.x).max(i32::from(clip.x));
    let right =
        (i32::from(area.x) + i32::from(area.width)).min(i32::from(clip.x) + i32::from(clip.width));
    let top = i32::from(area.y).max(i32::from(clip.y));
    let bottom = (i32::from(area.y) + i32::from(area.height))
        .min(i32::from(clip.y) + i32::from(clip.height));
    let width = UnicodeWidthStr::width(grapheme);
    let display_width = width.max(1) as i32;
    let grapheme_right = x.saturating_add(display_width);
    let overlaps = left < right && x < right && grapheme_right > left;
    let clipped_left = overlaps && x < left;
    let clipped_right = overlaps && grapheme_right > right;
    let draw_x = x.max(left);
    if overlaps && y >= top && y < bottom {
        if let Some(cell) = buffer.cell_mut((draw_x as u16, y as u16)) {
            cell.reset();
            if clipped_right || clipped_left {
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
        if !clipped_right && !clipped_left {
            for column in 1..display_width {
                if let Some(cell) = buffer.cell_mut(((x + column) as u16, y as u16)) {
                    cell.reset();
                    cell.set_style(style);
                }
            }
        }
    }
    if clipped_right { right } else { grapheme_right }
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

pub(crate) fn draw_styled_chars_clipped(
    buffer: &mut Buffer,
    clip: Rect,
    mut x: i32,
    y: i32,
    chars: impl Iterator<Item = (char, Style)>,
) -> i32 {
    for_each_grapheme(chars, |grapheme, style| {
        x = draw_grapheme_clipped(buffer, clip, x, y, style, grapheme);
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
    fn arbitrary_clip_replaces_partial_wide_graphemes() {
        let style = Style {
            fg: Color::Indexed(3),
            bg: Color::Indexed(4),
            modifiers: Modifier::UNDERLINED,
        };
        let mut buffer = Buffer::empty(Rect::new(2, 1, 6, 2));
        let clip = Rect::new(3, 1, 3, 1);
        draw_styled_chars_clipped(&mut buffer, clip, 2, 1, "日A日".chars().map(|c| (c, style)));
        assert_eq!(buffer[(2, 1)].symbol(), " ");
        assert_eq!(buffer[(3, 1)].symbol(), "�");
        assert_eq!(buffer[(4, 1)].symbol(), "A");
        assert_eq!(buffer[(5, 1)].symbol(), "�");
        assert_eq!(buffer[(6, 1)].symbol(), " ");
        assert_eq!(buffer[(3, 1)].fg, style.fg);
        assert_eq!(buffer[(5, 1)].bg, style.bg);
        assert!(buffer[(5, 1)].modifier.contains(Modifier::UNDERLINED));
        draw_styled_chars_clipped(&mut buffer, clip, 3, 2, "X".chars().map(|c| (c, style)));
        assert_eq!(buffer[(3, 2)].symbol(), " ");
    }

    #[test]
    fn clipping_multicolumn_graphemes_preserves_neighbors() {
        let style = Style {
            fg: Color::Indexed(3),
            bg: Color::Indexed(4),
            modifiers: Modifier::BOLD,
        };
        let clip = Rect::new(2, 0, 2, 1);
        let mut right = Buffer::empty(Rect::new(0, 0, 5, 1));
        right[(4, 0)].set_symbol(">");
        draw_styled_chars_clipped(&mut right, clip, 2, 0, "क्ष्मX".chars().map(|c| (c, style)));
        assert_eq!(right[(2, 0)].symbol(), "�");
        assert_eq!(right[(3, 0)].symbol(), " ");
        assert_eq!(right[(4, 0)].symbol(), ">");
        assert_eq!(right[(2, 0)].fg, style.fg);

        let mut left = Buffer::empty(Rect::new(0, 0, 5, 1));
        left[(1, 0)].set_symbol("<");
        left[(4, 0)].set_symbol(">");
        draw_styled_chars_clipped(&mut left, clip, 0, 0, "क्ष्मX".chars().map(|c| (c, style)));
        assert_eq!(left[(1, 0)].symbol(), "<");
        assert_eq!(left[(2, 0)].symbol(), "�");
        assert_eq!(left[(3, 0)].symbol(), "X");
        assert_eq!(left[(4, 0)].symbol(), ">");

        let mut both = Buffer::empty(Rect::new(0, 0, 5, 1));
        both[(3, 0)].set_symbol(">");
        draw_styled_chars_clipped(
            &mut both,
            Rect::new(2, 0, 1, 1),
            1,
            0,
            "क्ष्मX".chars().map(|c| (c, style)),
        );
        assert_eq!(both[(2, 0)].symbol(), "�");
        assert_eq!(both[(3, 0)].symbol(), ">");

        let mut full = Buffer::empty(Rect::new(0, 0, 5, 1));
        draw_styled_chars_clipped(
            &mut full,
            Rect::new(0, 0, 4, 1),
            0,
            0,
            "क्ष्मX".chars().map(|c| (c, style)),
        );
        assert_eq!(full[(0, 0)].symbol(), "क्ष्म");
        assert_eq!(full[(1, 0)].bg, style.bg);
        assert_eq!(full[(2, 0)].bg, style.bg);
        assert_eq!(full[(3, 0)].symbol(), "X");
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
