use libtiny_common::{MsgSource, TabStyle};
use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

use crate::text;
use crate::{
    config::{Colors, Style},
    messaging::MessagingUI,
};

pub(crate) struct Tab {
    pub(crate) visible_name: String,
    pub(crate) widget: MessagingUI,
    pub(crate) src: MsgSource,
    pub(crate) style: TabStyle,
    /// Alt-character to use to switch to this tab.
    pub(crate) switch: Option<char>,
}

pub(crate) fn tab_style(style: TabStyle, colors: &Colors) -> Style {
    match style {
        TabStyle::Normal => colors.tab_normal,
        TabStyle::JoinOrPart => colors.tab_joinpart,
        TabStyle::NewMsg => colors.tab_new_msg,
        TabStyle::Highlight => colors.tab_highlight,
    }
}

impl Tab {
    pub(crate) fn visible_name(&self) -> &str {
        &self.visible_name
    }

    pub(crate) fn set_style(&mut self, style: TabStyle) {
        self.style = style;
    }

    pub(crate) fn update_source<F>(&mut self, f: &F)
    where
        F: Fn(&mut MsgSource),
    {
        f(&mut self.src)
    }

    pub(crate) fn width(&self) -> i32 {
        text::text_width(self.visible_name()) as i32
    }

    pub(crate) fn draw(
        &self,
        buffer: &mut Buffer,
        colors: &Colors,
        clip: Rect,
        pos_x: i32,
        pos_y: i32,
        active: bool,
    ) {
        let style: Style = if active {
            colors.tab_active
        } else {
            tab_style(self.style, colors)
        };

        let mut switch_drawn = false;
        let chars = self.visible_name().chars().map(|ch| {
            let char_style = if Some(ch) == self.switch && !switch_drawn {
                switch_drawn = true;
                Style {
                    modifiers: style.modifiers | Modifier::UNDERLINED,
                    ..style
                }
            } else {
                style
            };
            (ch, char_style)
        });
        text::draw_styled_chars_clipped(buffer, clip, pos_x, pos_y, chars);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg_area::Layout;

    #[test]
    fn tab_draws_column_width_and_underlines_switch() {
        let tab = Tab {
            visible_name: "\u{3099}日A".into(),
            widget: MessagingUI::new(10, 3, 10, Layout::Compact),
            src: MsgSource::Serv {
                serv: "test".into(),
            },
            style: TabStyle::Normal,
            switch: Some('日'),
        };
        let colors = Colors::default();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        assert_eq!(tab.width(), 4);
        tab.draw(&mut buffer, &colors, Rect::new(0, 0, 4, 1), 0, 0, false);
        assert_eq!(buffer[(0, 0)].symbol(), "◌\u{3099}");
        assert_eq!(buffer[(1, 0)].symbol(), "日");
        assert_eq!(buffer[(3, 0)].symbol(), "A");
        assert!(buffer[(1, 0)].modifier.contains(Modifier::UNDERLINED));
        assert!(!buffer[(3, 0)].modifier.contains(Modifier::UNDERLINED));

        let mut narrow = Buffer::empty(Rect::new(0, 0, 2, 1));
        tab.draw(&mut narrow, &colors, Rect::new(0, 0, 2, 1), 0, 0, true);
        assert_eq!(narrow[(1, 0)].symbol(), "�");
        assert_eq!(narrow[(1, 0)].fg, colors.tab_active.fg);
        assert!(narrow[(1, 0)].modifier.contains(Modifier::UNDERLINED));
    }
}
