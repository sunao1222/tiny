use crate::config::{Colors, Style};
use crate::line_split::{LineDataCache, LineType};

use crate::text::draw_styled_chars;
use libtiny_wire::formatting::{Color, IrcFormatEvent, parse_irc_formatting};
use ratatui::buffer::Buffer;
use ratatui::style::{Color as TuiColor, Modifier};
use unicode_width::UnicodeWidthChar;

/// A single line added to the widget. May be rendered as multiple lines on the
/// screen.
#[derive(Debug)]
pub(crate) struct Line<'a> {
    segments: Vec<StyledString<'a>>,
    style: SegStyle,
    line_data: LineDataCache,
}

#[derive(Debug)]
struct StyledString<'a> {
    string: &'a str,
    style: SegStyle,
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum SegStyle {
    /// A specific style. Useful when rendering IRC colors (which should look
    /// the same across color schemes).
    Fixed(Style),

    /// An index to nick colors. Note that the index should be larger than size
    /// of the color list, so make sure to use mod.
    NickColor(usize),

    // Rest of the styles are from the color scheme
    UserMsg,
    ErrMsg,
    Topic,
    Join,
    Part,
    Nick,
    Faded,
    Highlight,
    Timestamp,
}

impl StyledString<'_> {
    pub(crate) fn style(&self, colors: &Colors) -> Style {
        use SegStyle::*;
        match self.style {
            Fixed(style) => style,
            NickColor(idx) => Style {
                fg: indexed_color(colors.nick[idx % colors.nick.len()]),
                bg: colors.user_msg.bg,
                modifiers: Modifier::empty(),
            },
            UserMsg => colors.user_msg,
            ErrMsg => colors.err_msg,
            Topic => colors.topic,
            Join => colors.join,
            Part => colors.part,
            Nick => colors.nick_change,
            Faded => colors.faded,
            Highlight => colors.highlight,
            Timestamp => colors.timestamp,
        }
    }
}

impl<'a> Line<'a> {
    pub(crate) fn new() -> Line<'a> {
        Line {
            segments: vec![],
            style: SegStyle::UserMsg,
            line_data: LineDataCache::msg_line(0, None),
        }
    }

    pub(crate) fn set_type(&mut self, line_type: LineType) {
        self.line_data.set_line_type(line_type)
    }

    pub(crate) fn line_type(&self) -> LineType {
        self.line_data.line_type()
    }

    pub(crate) fn swap_cache(&mut self, cache: &mut LineDataCache) {
        std::mem::swap(&mut self.line_data, cache);
    }

    fn set_message_style(&mut self, style: SegStyle) {
        self.style = style;
    }

    fn add_attr(&mut self, attr: Modifier) {
        let mut style = match self.style {
            SegStyle::Fixed(style) => style,
            _ => Style {
                fg: TuiColor::Reset,
                bg: TuiColor::Reset,
                modifiers: Modifier::empty(),
            },
        };
        style.modifiers.insert(attr);
        self.set_message_style(SegStyle::Fixed(style));
    }

    /// Add the text to the line. The text can contain IRC formatting characters. The text should
    /// not contain other control characters like '\n' or '\r'.
    pub(crate) fn add_text(&mut self, str: &'a str, style: SegStyle) {
        self.add_text_inner(str, style, false);
    }

    pub(crate) fn add_independent_text(&mut self, str: &'a str, style: SegStyle) {
        self.add_text_inner(str, style, true);
    }

    fn add_text_inner(&mut self, str: &'a str, style: SegStyle, mut independent: bool) {
        self.set_message_style(style);

        for format_event in parse_irc_formatting(str) {
            match format_event {
                IrcFormatEvent::Bold => self.add_attr(Modifier::BOLD),
                IrcFormatEvent::Italic => self.add_attr(Modifier::ITALIC),
                IrcFormatEvent::Underline => self.add_attr(Modifier::UNDERLINED),
                IrcFormatEvent::Strikethrough => self.add_attr(Modifier::CROSSED_OUT),
                IrcFormatEvent::Monospace => {
                    // TODO
                }
                IrcFormatEvent::Text(text) => {
                    if independent && let Some(ch) = text.chars().next() {
                        if ch.width() == Some(0) {
                            self.segments.push(StyledString {
                                string: "◌",
                                style: self.style,
                            });
                        }
                        independent = false;
                    }
                    self.segments.push(StyledString {
                        string: text,
                        style: self.style,
                    });
                }
                IrcFormatEvent::Color { fg, bg } => {
                    let style = SegStyle::Fixed(Style {
                        fg: irc_color_to_tui(fg),
                        bg: bg.map(irc_color_to_tui).unwrap_or(TuiColor::Reset),
                        modifiers: Modifier::empty(),
                    });

                    self.set_message_style(style);
                }
                IrcFormatEvent::ReverseColor => {
                    if let SegStyle::Fixed(Style { fg, bg, modifiers }) = self.style {
                        self.set_message_style(SegStyle::Fixed(Style {
                            fg: bg,
                            bg: fg,
                            modifiers,
                        }));
                    }
                }
                // TODO: ResetColors should only reset the colors and not the formatting.
                // However we don't store the current formatting and we can't apply formatting to
                // a color from the current color scheme (e.g. `SegStyle::UserMsg`). For now reset
                // the colors and formatting.
                IrcFormatEvent::ResetColors | IrcFormatEvent::Reset => {
                    self.set_message_style(SegStyle::UserMsg);
                }
            }
        }

        self.line_data.set_dirty();
    }

    /// Calculates the number of lines that this line will be.
    /// The calculation is only done if the line_data is dirty or the window is resized.
    pub(crate) fn rendered_height(&mut self, width: i32) -> i32 {
        let msg_padding = self.line_type().msg_padding();
        if self.line_data.is_dirty() || self.line_data.needs_resize(width, 0, msg_padding) {
            self.line_data = LineDataCache::msg_line(width, msg_padding);
            let mut full_line = self.segments.iter().flat_map(|s| s.string.chars());
            self.line_data.calculate_height(&mut full_line, 0);
        }
        self.line_data.get_line_count().unwrap() as i32
    }

    pub(crate) fn draw(
        &self,
        buffer: &mut Buffer,
        colors: &Colors,
        pos_x: i32,
        pos_y: i32,
        first_line: i32,
        height: i32,
    ) {
        let mut chars = self.segments.iter().flat_map(|seg| {
            let style = seg.style(colors);
            seg.string.chars().map(move |c| (c, style))
        });
        let mut start = 0usize;
        let mut ends = self
            .line_data
            .get_splits()
            .iter()
            .map(|&i| i as usize)
            .chain(std::iter::once(usize::MAX));
        for line_num in 0..height {
            let Some(end) = ends.next() else { break };
            let count = end.saturating_sub(start);
            let mut row = chars.by_ref().take(count).peekable();
            // A whitespace scalar at a wrapping boundary is omitted unless it
            // is the base of a combining sequence.
            let first = row.next();
            let first = first.filter(|(c, _)| {
                line_num == 0
                    || !c.is_whitespace()
                    || row.peek().is_some_and(|(next, _)| next.width() == Some(0))
            });
            if line_num >= first_line {
                let x = pos_x
                    + if line_num == 0 {
                        0
                    } else {
                        self.line_data.new_line_offset()
                    };
                draw_styled_chars(buffer, x, pos_y + line_num, first.into_iter().chain(row));
            } else {
                // The iterator still has to advance for rows above the viewport.
                row.for_each(drop);
            }
            start = end;
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

fn indexed_color(index: u8) -> TuiColor {
    if index == 0 {
        TuiColor::Reset
    } else {
        TuiColor::Indexed(index)
    }
}

fn irc_color_to_tui(irc_color: Color) -> TuiColor {
    let index = match irc_color {
        Color::White => 255,
        Color::Black => 16,
        Color::Blue => 21,
        Color::Green => 46,
        Color::Red => 196,
        Color::Brown => 88,
        Color::Magenta => 93,
        Color::Orange => 210,
        Color::Yellow => 228,
        Color::LightGreen => 154,
        Color::Cyan => 75,
        Color::LightCyan => 39,
        Color::LightBlue => 38,
        Color::Pink => 129,
        Color::Grey => 243,
        Color::LightGrey => 249,
        Color::Default => 0,
        Color::Ansi(ansi_color) => ansi_color,
    };
    indexed_color(index)
}

////////////////////////////////////////////////////////////////////////////////

#[test]
fn height_test_1() {
    let mut line = Line::new();
    line.add_text("a b c d e", SegStyle::UserMsg);
    assert_eq!(line.rendered_height(1), 9);
    assert_eq!(line.rendered_height(2), 5);
    assert_eq!(line.rendered_height(3), 3);
    assert_eq!(line.rendered_height(4), 3);
    assert_eq!(line.rendered_height(5), 2);
    assert_eq!(line.rendered_height(6), 2);
    assert_eq!(line.rendered_height(7), 2);
    assert_eq!(line.rendered_height(8), 2);
    assert_eq!(line.rendered_height(9), 1);
}

#[test]
fn height_test_2() {
    let mut line = Line::new();
    line.add_text("ab c d e", SegStyle::UserMsg);
    assert_eq!(line.rendered_height(1), 8);
    assert_eq!(line.rendered_height(2), 4);
    assert_eq!(line.rendered_height(3), 3);
    assert_eq!(line.rendered_height(4), 2);
    assert_eq!(line.rendered_height(5), 2);
    assert_eq!(line.rendered_height(6), 2);
    assert_eq!(line.rendered_height(7), 2);
    assert_eq!(line.rendered_height(8), 1);
}

#[test]
fn height_test_3() {
    let mut line = Line::new();
    line.add_text("ab cd e", SegStyle::UserMsg);
    assert_eq!(line.rendered_height(1), 7);
    assert_eq!(line.rendered_height(2), 4);
    assert_eq!(line.rendered_height(3), 3);
    assert_eq!(line.rendered_height(4), 2);
    assert_eq!(line.rendered_height(5), 2);
    assert_eq!(line.rendered_height(6), 2);
    assert_eq!(line.rendered_height(7), 1);
}

#[test]
fn height_test_4() {
    let mut line = Line::new();
    line.add_text("ab cde", SegStyle::UserMsg);
    assert_eq!(line.rendered_height(1), 6);
    assert_eq!(line.rendered_height(2), 4);
    assert_eq!(line.rendered_height(3), 2);
    assert_eq!(line.rendered_height(4), 2);
    assert_eq!(line.rendered_height(5), 2);
    assert_eq!(line.rendered_height(6), 1);
}

#[test]
fn height_test_5() {
    use std::fs::File;
    use std::io::Read;

    let text: String = {
        let mut text = String::new();
        let mut single_line = String::new();
        let mut file = File::open("test/lipsum.txt").unwrap();
        file.read_to_string(&mut text).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 102); // make sure we did it right
        for (line_idx, line) in lines.iter().enumerate() {
            single_line.push_str(line);
            if line_idx != lines.len() - 1 {
                single_line.push(' ');
            }
        }
        single_line
    };

    let mut line = Line::new();
    line.add_text(&text, SegStyle::UserMsg);
    // lipsum.txt has 1160 words in it. each line should contain at most one
    // word so we should have 1160 lines.
    assert_eq!(line.rendered_height(80), 102);
}

#[test]
fn align_test() {
    let mut line = Line::new();
    line.set_type(LineType::AlignedMsg { msg_padding: 1 });
    /*
    123
     45
     67
     8
    */
    line.add_text("12345678", SegStyle::UserMsg);

    assert_eq!(line.rendered_height(3), 4);
}

#[test]
fn unicode_drawing_wraps_by_columns_and_keeps_styled_marks() {
    let mut line = Line::new();
    line.add_text("Aか", SegStyle::UserMsg);
    line.add_text("\u{3099}日B", SegStyle::Highlight);
    assert_eq!(line.rendered_height(3), 2);
    let colors = Colors::default();
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 3, 2));
    line.draw(&mut buffer, &colors, 0, 0, 0, 2);
    assert_eq!(buffer[(0, 0)].symbol(), "A");
    assert_eq!(buffer[(1, 0)].symbol(), "か\u{3099}");
    assert_eq!(buffer[(1, 0)].fg, colors.user_msg.fg);
    assert_eq!(buffer[(0, 1)].symbol(), "日");
    assert_eq!(buffer[(2, 1)].symbol(), "B");
}

#[test]
fn grapheme_across_styles_wraps_as_one_symbol() {
    let mut line = Line::new();
    line.add_text("A👩", SegStyle::UserMsg);
    line.add_text("\u{200d}💻B", SegStyle::Highlight);
    assert_eq!(line.rendered_height(3), 2);
    let colors = Colors::default();
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 3, 2));
    line.draw(&mut buffer, &colors, 0, 0, 0, 2);
    assert_eq!(buffer[(1, 0)].symbol(), "👩‍💻");
    assert_eq!(buffer[(1, 0)].fg, colors.user_msg.fg);
    assert_eq!(buffer[(0, 1)].symbol(), "B");
}

#[test]
fn unicode_drawing_handles_narrow_and_scrolled_lines() {
    let mut line = Line::new();
    line.add_text("\u{3099}日B", SegStyle::UserMsg);
    assert_eq!(line.rendered_height(1), 3);
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 3));
    line.draw(&mut buffer, &Colors::default(), 0, 0, 1, 3);
    assert_eq!(buffer[(0, 0)].symbol(), " ");
    assert_eq!(buffer[(0, 1)].symbol(), "�");
    assert_eq!(buffer[(0, 2)].symbol(), "B");

    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 3));
    line.draw(&mut buffer, &Colors::default(), 0, 0, 0, 3);
    assert_eq!(buffer[(0, 0)].symbol(), "◌\u{3099}");
}
