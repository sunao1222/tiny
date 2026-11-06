/// Cache that stores the state of a line's height calculation.
/// `line_count` is used as the dirty bit to invalidate the cache.
#[derive(Clone, Debug)]
pub(crate) struct LineDataCache {
    /// Indices to split on when we draw, not always whitespaces
    split_indices: Vec<i32>,
    /// The total number of lines (height) that will be rendered
    line_count: Option<i32>,
    /// The current width of InputArea. Used in determining if we need to invalidate due to resize.
    width: i32,
    /// The type of line we're trying to calculate height for
    line_type: LineType,
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum LineType {
    Input {
        /// Current nickname length. Used in determining if we need to invalidate due to resize.
        nick_length: usize,
    },
    AlignedMsg {
        /// The offset of timestamp + nick on multiline messages
        msg_padding: usize,
    },
    /// A regular line
    Msg,
}

impl LineType {
    pub(crate) fn msg_padding(&self) -> Option<usize> {
        if let LineType::AlignedMsg { msg_padding } = self {
            Some(*msg_padding)
        } else {
            None
        }
    }
}

impl LineDataCache {
    pub(crate) fn input_line(width: i32, nick_length: usize) -> LineDataCache {
        LineDataCache {
            split_indices: Vec::new(),
            line_count: None,
            width,
            line_type: LineType::Input { nick_length },
        }
    }

    pub(crate) fn msg_line(width: i32, msg_padding: Option<usize>) -> LineDataCache {
        let line_type = if let Some(msg_padding) = msg_padding {
            LineType::AlignedMsg { msg_padding }
        } else {
            LineType::Msg
        };
        LineDataCache {
            split_indices: Vec::new(),
            line_count: None,
            width,
            line_type,
        }
    }

    pub(crate) fn line_type(&self) -> LineType {
        self.line_type
    }

    pub(crate) fn set_line_type(&mut self, line_type: LineType) {
        self.line_type = line_type
    }

    /// Performs a check to see if the width or nickname length changed
    /// which would require an invalidation of the cache and recalculation of
    /// the line height.
    pub(crate) fn needs_resize(&self, width: i32, nick_len: usize, msg_pad: Option<usize>) -> bool {
        // TODO so ugly
        self.width != width
            || match (self.line_type, msg_pad) {
                (LineType::Input { nick_length }, ..) => nick_len != nick_length,
                (LineType::AlignedMsg { msg_padding }, Some(pad)) => pad != msg_padding,
                _ => false,
            }
    }

    /// Sets `line_count` to `None`, which invalidates the cache.
    pub(crate) fn set_dirty(&mut self) {
        self.line_count = None;
    }

    /// Checks if the cache is invalidated by seeing if
    /// `line_count` is `None`.
    pub(crate) fn is_dirty(&self) -> bool {
        self.line_count.is_none()
    }

    pub(crate) fn get_line_count(&self) -> Option<usize> {
        self.line_count.map(|c| c as usize)
    }

    pub(crate) fn get_splits(&self) -> &[i32] {
        &self.split_indices
    }

    pub(crate) fn new_line_offset(&self) -> i32 {
        match self.line_type {
            LineType::Input { .. } | LineType::Msg => 0,
            LineType::AlignedMsg { msg_padding } => msg_padding as i32,
        }
    }

    /// Returns the width of lines that are not the first line
    fn multi_line_width(&self) -> i32 {
        match self.line_type {
            // expand to full width
            LineType::Input { .. } | LineType::Msg => self.width,
            // consecutive lines can apply an offset
            LineType::AlignedMsg { msg_padding } => self.width - msg_padding as i32,
        }
    }

    /// Usable columns on a rendered line, including the tiny-terminal fallback.
    pub(crate) fn display_width(&self, line: i32) -> i32 {
        (if line == 0 {
            match self.line_type {
                LineType::Input { nick_length } => self.width - nick_length as i32,
                _ => self.width,
            }
        } else {
            self.multi_line_width()
        })
        .max(1)
    }

    /// Recalculate wrapping from the complete text, keeping scalar-indexed
    /// split points at grapheme boundaries. The offset is retained for callers
    /// but cannot safely skip text because appends can extend a grapheme.
    pub(crate) fn calculate_height<I: Iterator<Item = char>>(&mut self, buffer: I, _offset: usize) {
        use unicode_width::UnicodeWidthStr;
        self.split_indices.clear();
        let mut line_width = self.display_width(0);
        let mut length = 0;
        let mut word_width = 0;
        let mut last_whitespace_end = None;
        let mut scalar_index = 0;
        let mut count = 1;
        crate::text::for_each_grapheme(buffer.map(|ch| (ch, ())), |grapheme, ()| {
            let whitespace = grapheme.chars().next().is_some_and(char::is_whitespace);
            let width = UnicodeWidthStr::width(grapheme).max(1) as i32;
            let effective = width.min(line_width);
            if length + effective > line_width {
                let next_width = self.multi_line_width().max(1);
                let next_char_width = width.min(next_width);
                let next_word_width = word_width + next_char_width;
                let split = if !whitespace && next_word_width <= next_width {
                    last_whitespace_end.unwrap_or(scalar_index)
                } else {
                    scalar_index
                };
                self.split_indices.push(split);
                count += 1;
                line_width = next_width;
                length = if split == scalar_index {
                    next_char_width
                } else {
                    next_word_width
                };
                last_whitespace_end = None;
                word_width = if whitespace { 0 } else { length };
            } else {
                length += effective;
                if !whitespace {
                    word_width += effective;
                }
            }
            scalar_index += grapheme.chars().count() as i32;
            if whitespace {
                last_whitespace_end = Some(scalar_index);
                word_width = 0;
            }
        });
        if matches!(self.line_type, LineType::Input { .. }) && length == line_width {
            count += 1;
        }
        self.line_count = Some(count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_only_at_grapheme_boundaries() {
        let text = "Aか\u{3099}👩‍💻B";
        let mut cache = LineDataCache::msg_line(3, None);
        cache.calculate_height(text.chars(), 0);
        assert_eq!(cache.get_splits(), &[3]);
        assert_eq!(cache.get_line_count(), Some(2));
        let mut narrow = LineDataCache::input_line(1, 0);
        narrow.calculate_height("👩‍💻X".chars(), 0);
        assert_eq!(narrow.get_splits(), &[3]);
        assert_eq!(narrow.get_line_count(), Some(3));
    }

    #[test]
    fn word_wrap_and_recalculation() {
        let mut cache = LineDataCache::msg_line(4, None);
        cache.calculate_height("ab cde".chars(), 0);
        assert_eq!(cache.get_splits(), &[3]);
        cache.calculate_height("ab cde!".chars(), 6);
        assert_eq!(cache.get_splits(), &[3]);
    }
}
