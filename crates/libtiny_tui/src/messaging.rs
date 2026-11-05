use termbox_simple::Termbox;

use std::convert::From;

use time::{self, Tm};
use unicode_width::UnicodeWidthChar;

use crate::config::Colors;
use crate::exit_dialogue::ExitDialogue;
use crate::input_area::InputArea;
use crate::key_map::KeyAction;
use crate::msg_area::line::SegStyle;
use crate::msg_area::{ActivityEvent, HistoryEntry, Layout, MsgArea};
use crate::trie::Trie;
use crate::widget::WidgetRet;

pub(crate) fn aligned_nick(nick: &str, columns: usize) -> String {
    let width = crate::termbox::text_width(nick);
    // Padding and preceding message segments must not become the base of a leading mark.
    let dotted = if nick
        .chars()
        .next()
        .is_some_and(|ch| UnicodeWidthChar::width(ch) == Some(0))
    {
        "◌"
    } else {
        ""
    };
    if width <= columns {
        return format!("{}{}{}", " ".repeat(columns - width), dotted, nick);
    }
    if columns == 0 {
        return String::new();
    }

    // Keep every zero-width character following a retained base, even across the cutoff.
    let available = columns - 1; // The ellipsis occupies one column.
    let mut used = 0;
    let mut end = 0;
    let mut has_base = false;
    for (i, ch) in nick.char_indices() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1);
        let cost = if width == 0 && !has_base { 1 } else { width };
        if used + cost > available {
            break;
        }
        used += cost;
        has_base = true;
        end = i + ch.len_utf8();
    }
    format!(
        "{}{}{}…",
        " ".repeat(available - used),
        if end > 0 { dotted } else { "" },
        &nick[..end]
    )
}

#[cfg(test)]
mod aligned_nick_tests {
    use super::aligned_nick;

    #[test]
    fn aligns_and_truncates_on_column_boundaries() {
        assert_eq!(aligned_nick("日a", 5), "  日a");
        assert_eq!(aligned_nick("日本語", 5), "日本…");
        assert_eq!(aligned_nick("日本語", 4), " 日…");
        assert_eq!(aligned_nick("日本語", 1), "…");
        assert_eq!(aligned_nick("日本語", 0), "");
        assert_eq!(aligned_nick("abcde", 4), "abc…");
        assert_eq!(aligned_nick("か\u{3099}日a", 4), " か\u{3099}…");
        assert_eq!(aligned_nick("\u{3099}日a", 2), "◌\u{3099}…");
    }
}

/// An input field and an area for showing messages and activities of a tab (channel, server,
/// mentions tab).
pub(crate) struct MessagingUI {
    /// The area showing the messages and activities.
    msg_area: MsgArea,

    /// The input field. `exit_dialogue` handles the input when available.
    // Two fields (instead of an enum) to avoid borrowchk problems.
    input_field: InputArea,

    exit_dialogue: Option<ExitDialogue>,

    /// Width of the UI, in characters.
    width: i32,

    /// Height of the UI, in lines.
    height: i32,

    /// All nicks in the channel. Used in autocompletion.
    nicks: Trie,

    /// Last timestamp added to the UI.
    last_ts: Option<Timestamp>,
}

/// Length of ": " suffix of nicks in messages
pub(crate) const MSG_NICK_SUFFIX_LEN: usize = 2;

/// Like `time::Tm`, but we only care about hour and minute parts.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) struct Timestamp {
    hour: i32,
    min: i32,
}

impl Timestamp {
    /// The width of a timestamp plus a space.
    pub(crate) const WIDTH: usize = 6;

    /// Spaces for a timestamp slot in aligned layout.
    pub(crate) const BLANK: &'static str = "      ";

    pub(crate) fn stamp(&self) -> String {
        format!("{:02}:{:02} ", self.hour, self.min)
    }
}

impl From<Tm> for Timestamp {
    fn from(tm: Tm) -> Timestamp {
        Timestamp {
            hour: tm.tm_hour,
            min: tm.tm_min,
        }
    }
}

impl MessagingUI {
    pub(crate) fn new(
        width: i32,
        height: i32,
        scrollback: usize,
        msg_layout: Layout,
    ) -> MessagingUI {
        MessagingUI {
            msg_area: MsgArea::new(width, height - 1, scrollback, msg_layout),
            input_field: InputArea::new(width, get_input_field_max_height(height)),
            exit_dialogue: None,
            width,
            height,
            nicks: Trie::new(),
            last_ts: None,
        }
    }
    pub(crate) fn apply_config(&mut self, scrollback: usize, layout: Layout) {
        self.msg_area.apply_config(scrollback, layout);
    }

    pub(crate) fn set_nick(&mut self, nick: String) {
        let nick_color = self.get_nick_color(&nick);
        self.input_field.set_nick(nick, nick_color);
        // update text field size
        let w = self.width;
        let h = self.height;
        self.resize(w, h);
    }

    pub(crate) fn get_nick(&self) -> Option<String> {
        self.input_field.get_nick()
    }

    pub(crate) fn draw(&mut self, tb: &mut Termbox, colors: &Colors, pos_x: i32, pos_y: i32) {
        match &self.exit_dialogue {
            Some(exit_dialogue) => {
                exit_dialogue.draw(tb, colors, pos_x, self.height - 1);
            }
            None => {
                // Draw InputArea first because it can trigger a resize of MsgArea
                self.input_field
                    .draw(tb, colors, pos_x, pos_y, self.height, &mut self.msg_area);
            }
        }
        self.msg_area.draw(tb, colors, pos_x, pos_y);
    }

    pub(crate) fn keypressed(
        &mut self,
        key_action: &KeyAction,
        command_completions: &Trie,
    ) -> WidgetRet {
        match key_action {
            KeyAction::Exit => {
                self.toggle_exit_dialogue();
                WidgetRet::KeyHandled
            }
            KeyAction::MessagesPageUp => {
                self.msg_area.page_up();
                WidgetRet::KeyHandled
            }
            KeyAction::MessagesPageDown => {
                self.msg_area.page_down();
                WidgetRet::KeyHandled
            }
            KeyAction::MessagesScrollUp => {
                self.msg_area.scroll_up();
                WidgetRet::KeyHandled
            }
            KeyAction::MessagesScrollDown => {
                self.msg_area.scroll_down();
                WidgetRet::KeyHandled
            }
            KeyAction::MessagesScrollTop => {
                self.msg_area.scroll_top();
                WidgetRet::KeyHandled
            }
            KeyAction::MessagesScrollBottom => {
                self.msg_area.scroll_bottom();
                WidgetRet::KeyHandled
            }
            KeyAction::InputAutoComplete => {
                if self.exit_dialogue.is_none() {
                    self.input_field
                        .autocomplete(&self.nicks, command_completions);
                }
                WidgetRet::KeyHandled
            }
            key_action => {
                let ret = {
                    if let Some(exit_dialogue) = self.exit_dialogue.as_ref() {
                        exit_dialogue.keypressed(key_action)
                    } else {
                        self.input_field.keypressed(key_action)
                    }
                };

                if let WidgetRet::Remove = ret {
                    self.exit_dialogue = None;
                    WidgetRet::KeyHandled
                } else {
                    ret
                }
            }
        }
    }

    pub(crate) fn resize(&mut self, width: i32, height: i32) {
        self.width = width;
        self.height = height;

        self.input_field
            .resize(width, get_input_field_max_height(height));
        // msg_area should resize based on input_field's rendered height
        let msg_area_height = height - self.input_field.get_height(width);
        self.msg_area.resize(width, msg_area_height);

        // We don't show the nick in exit dialogue, so it has the full width
        if let Some(exit_dialogue) = &mut self.exit_dialogue {
            exit_dialogue.resize(width);
        }
    }

    /// Get contents of the input field and cursor location and clear it.
    pub(crate) fn flush_input_field(&mut self) -> (String, i32) {
        self.input_field.flush()
    }

    /// Add a line to input field history.
    pub(crate) fn add_input_field_history(&mut self, str: &str) {
        self.input_field.add_history(str)
    }

    /// Set input field contents.
    pub(crate) fn set_input_field(&mut self, str: &str) {
        self.input_field.set(str)
    }

    /// Set cursor location in the input field.
    pub(crate) fn set_cursor(&mut self, cursor: i32) {
        self.input_field.set_cursor(cursor);
    }

    fn toggle_exit_dialogue(&mut self) {
        if self.exit_dialogue.take().is_none() {
            // We don't show the nick in exit dialogue, so it has the full width
            self.exit_dialogue = Some(ExitDialogue::new(self.width));
        }
    }
}

/// Calculation for input field's maximum height
fn get_input_field_max_height(window_height: i32) -> i32 {
    window_height / 2
}

////////////////////////////////////////////////////////////////////////////////
// Adding new messages

impl MessagingUI {
    fn show_timestamp(&mut self, ts: Timestamp) -> bool {
        let show = self.last_ts != Some(ts);
        self.last_ts = Some(ts);
        show
    }

    pub(crate) fn show_topic(&mut self, topic: &str, ts: Timestamp) {
        let show_timestamp = self.show_timestamp(ts);
        self.msg_area.push(HistoryEntry::Topic {
            timestamp: ts,
            show_timestamp,
            text: topic.to_owned(),
        });
    }

    pub(crate) fn add_client_err_msg(&mut self, msg: &str) {
        self.msg_area.push(HistoryEntry::Client {
            text: msg.to_owned(),
            style: SegStyle::ErrMsg,
        });
    }

    pub(crate) fn add_client_notify_msg(&mut self, msg: &str) {
        self.msg_area.push(HistoryEntry::Client {
            text: msg.to_owned(),
            style: SegStyle::Faded,
        });
    }

    pub(crate) fn add_client_msg(&mut self, msg: &str) {
        self.msg_area.push(HistoryEntry::Client {
            text: msg.to_owned(),
            style: SegStyle::UserMsg,
        });
    }

    pub(crate) fn add_privmsg(
        &mut self,
        sender: &str,
        msg: &str,
        ts: Timestamp,
        highlight: bool,
        is_action: bool,
    ) {
        // HACK: Some servers (bridges) don't send RPL_NAMREPLY and JOIN/PART messages but we still
        // want to support tab completion on those servers, so when we see a message from someone
        // we add the user to the nick list so that tab completion will complete their nick. See
        // #253 for details.
        self.nicks.insert(sender);

        let show_timestamp = self.show_timestamp(ts);
        self.msg_area.push(HistoryEntry::Privmsg {
            timestamp: ts,
            show_timestamp,
            sender: sender.to_owned(),
            text: msg.to_owned(),
            highlight,
            is_action,
        });
    }

    pub(crate) fn add_msg(&mut self, msg: &str, ts: Timestamp) {
        let show_timestamp = self.show_timestamp(ts);
        self.msg_area.push(HistoryEntry::Message {
            timestamp: ts,
            show_timestamp,
            text: msg.to_owned(),
            is_error: false,
        });
    }

    pub(crate) fn add_err_msg(&mut self, msg: &str, ts: Timestamp) {
        let show_timestamp = self.show_timestamp(ts);
        self.msg_area.push(HistoryEntry::Message {
            timestamp: ts,
            show_timestamp,
            text: msg.to_owned(),
            is_error: true,
        });
    }

    pub(crate) fn clear(&mut self) {
        self.msg_area.clear();
        self.last_ts = None;
    }

    fn get_nick_color(&self, sender: &str) -> usize {
        nick_color(sender)
    }
}

pub(crate) fn nick_color(sender: &str) -> usize {
    let mut hash: usize = 5381;
    for c in sender.chars() {
        hash = hash.wrapping_mul(33).wrapping_add(c as usize);
    }
    hash
}

////////////////////////////////////////////////////////////////////////////////
// Keeping nick list up-to-date

impl MessagingUI {
    pub(crate) fn clear_nicks(&mut self) {
        self.nicks.clear();
    }

    pub(crate) fn join(&mut self, nick: &str, ts: Option<Timestamp>, ignore: bool) {
        self.nicks.insert(nick);

        if !ignore && let Some(ts) = ts {
            self.add_activity(ts, ActivityEvent::Join(nick.to_owned()));
        }
    }

    pub(crate) fn part(&mut self, nick: &str, ts: Option<Timestamp>, ignore: bool) {
        self.nicks.remove(nick);

        if !ignore && let Some(ts) = ts {
            self.add_activity(ts, ActivityEvent::Part(nick.to_owned()));
        }
    }

    pub(crate) fn nick(&mut self, old_nick: &str, new_nick: &str, ts: Timestamp) {
        self.nicks.remove(old_nick);
        self.nicks.insert(new_nick);

        self.add_activity(
            ts,
            ActivityEvent::Nick {
                old: old_nick.to_owned(),
                new: new_nick.to_owned(),
            },
        );
    }

    fn add_activity(&mut self, ts: Timestamp, event: ActivityEvent) {
        if let Err(event) = self.msg_area.append_activity(ts, event) {
            let show_timestamp = self.show_timestamp(ts);
            self.msg_area.push(HistoryEntry::Activity {
                timestamp: ts,
                show_timestamp,
                events: vec![event],
            });
        }
    }
}
