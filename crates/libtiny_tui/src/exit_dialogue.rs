use ratatui::buffer::Buffer;

use crate::key_map::KeyAction;
use crate::text::draw_chars;
use crate::{config::Colors, widget::WidgetRet};

#[derive(Debug)]
pub(crate) struct ExitDialogue {
    width: i32,
}

impl ExitDialogue {
    pub(crate) fn new(width: i32) -> ExitDialogue {
        ExitDialogue { width }
    }
}

static MSG: &str = "Really quit? (Enter for yes, Esc to cancel)";

impl ExitDialogue {
    pub(crate) fn resize(&mut self, width: i32) {
        self.width = width;
    }

    pub(crate) fn draw(&self, buffer: &mut Buffer, colors: &Colors, pos_x: i32, pos_y: i32) {
        let width = self.width.max(0) as usize;
        let chars = MSG.chars().chain(std::iter::repeat(' ')).take(width);
        draw_chars(buffer, pos_x, pos_y, colors.exit_dialogue, chars);
    }

    pub(crate) fn keypressed(&self, key_action: &KeyAction) -> WidgetRet {
        match key_action {
            KeyAction::Input('y') | KeyAction::InputSend => WidgetRet::Command("quit".to_string()),
            _ => WidgetRet::Remove,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn dialogue_clips_and_fills_its_width() {
        let colors = Colors::default();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 50, 1));
        ExitDialogue::new(3).draw(&mut buffer, &colors, 1, 0);
        assert_eq!(buffer[(1, 0)].symbol(), "R");
        assert_eq!(buffer[(3, 0)].symbol(), "a");
        assert_eq!(buffer[(4, 0)].symbol(), " ");
        assert_eq!(buffer[(3, 0)].bg, colors.exit_dialogue.bg);

        ExitDialogue::new(48).draw(&mut buffer, &colors, 1, 0);
        assert_eq!(buffer[(48, 0)].symbol(), " ");
        assert_eq!(buffer[(48, 0)].bg, colors.exit_dialogue.bg);
    }
}
