use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use libtiny_common::{ChanNameRef, MsgTarget};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

use crate::msg_area::Layout;
use crate::tui::{TUI, TUIRet};

#[test]
fn aligned_messages_wrap_using_display_columns() {
    let mut tui = TUI::new_test(20, 8);
    tui.set_layout(Layout::Aligned { max_nick_len: 6 });
    let serv = "s";
    let chan = ChanNameRef::new("#日本");
    tui.new_server_tab(serv, None);
    tui.set_nick(serv, "test");
    tui.new_chan_tab(serv, chan);
    tui.next_tab();
    tui.next_tab();
    let target = MsgTarget::Chan { serv, chan };
    tui.add_privmsg(
        "日本語X",
        "A中B 한글",
        time::at_utc(time::Timespec::new(0, 0)),
        &target,
        false,
        false,
    );
    tui.draw();
    let buffer = tui.get_front_buffer();
    let rows: Vec<Vec<&str>> = (0..8)
        .map(|y| (0..20).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();
    let first = rows
        .iter()
        .position(|row| row[0..5] == ["0", "0", ":", "0", "0"])
        .unwrap();
    assert_eq!(&rows[first][6..12], &[" ", "日", " ", "本", " ", "…"]);
    assert_eq!(&rows[first][14..19], &["A", "中", " ", "B", " "]);
    assert_eq!(&rows[first + 1][14..18], &["한", " ", "글", " "]);
}

#[test]
fn unicode_editing_resize_history_and_send_preserve_graphemes() {
    let mut tui = TUI::new_test(42, 10);
    tui.new_server_tab("s", None);
    tui.set_nick("s", "test");
    tui.next_tab();
    for ch in "Aか\u{3099}👩‍💻B".chars() {
        tui.handle_input_event(key(KeyCode::Char(ch)));
    }
    tui.draw();
    let buffer = tui.get_front_buffer();
    assert_eq!(buffer[(6, 6)].symbol(), "A");
    assert_eq!(buffer[(7, 6)].symbol(), "か\u{3099}");
    assert_eq!(buffer[(9, 6)].symbol(), "👩‍💻");
    let text = crate::test_utils::buffer_str(&buffer, 42, 10);
    assert!(
        text.lines()
            .nth(6)
            .unwrap()
            .starts_with("test: Aか\u{3099}👩‍💻B"),
        "{text}"
    );

    tui.handle_input_event(key(KeyCode::Left));
    tui.handle_input_event(key(KeyCode::Backspace));
    tui.draw();
    let redrawn = tui.get_front_buffer();
    assert_eq!(redrawn[(9, 6)].symbol(), "B");
    assert_eq!(redrawn[(10, 6)].symbol(), " ");
    tui.set_size(12, 10);
    tui.draw();
    let sent = tui.handle_input_event(key(KeyCode::Enter));
    match sent {
        Some(TUIRet::Input { msg, .. }) => {
            assert_eq!(msg, "Aか\u{3099}B")
        }
        other => panic!("unexpected send result: {other:?}"),
    }

    tui.handle_input_event(key(KeyCode::Up));
    tui.draw();
    let sent = tui.handle_input_event(key(KeyCode::Enter));
    match sent {
        Some(TUIRet::Input { msg, .. }) => {
            assert_eq!(msg, "Aか\u{3099}B")
        }
        other => panic!("unexpected history result: {other:?}"),
    }
}

#[test]
fn resizing_with_an_oversized_unicode_tab_keeps_input_and_navigation_working() {
    let mut tui = TUI::new_test(40, 6);
    tui.new_server_tab("s", Some("日本日本日本".into()));
    tui.next_tab();

    tui.set_size(10, 6);
    tui.draw();
    let buffer = tui.get_front_buffer();
    assert!((0..10).any(|x| buffer[(x, 3)].symbol() == "日"));
    assert!((0..10).any(|x| buffer[(x, 3)].symbol() == "本"));

    for ch in "hello".chars() {
        tui.handle_input_event(key(KeyCode::Char(ch)));
    }
    match tui.handle_input_event(key(KeyCode::Enter)) {
        Some(TUIRet::Input { msg, .. }) => assert_eq!(msg, "hello"),
        other => panic!("unexpected input result after resize: {other:?}"),
    }

    tui.prev_tab();
    tui.next_tab();
    tui.set_size(3, 6);
    tui.prev_tab();
    tui.next_tab();
    tui.set_size(40, 6);
    tui.draw();
    let buffer = tui.get_front_buffer();
    assert_eq!(
        (0..40).filter(|&x| buffer[(x, 3)].symbol() == "日").count(),
        3
    );
}

#[test]
fn message_initial_combining_marks_do_not_attach_to_nick_separator() {
    for layout in [Layout::Compact, Layout::Aligned { max_nick_len: 6 }] {
        for msg in ["\u{301}a", "\u{2}\u{301}a"] {
            let mut tui = TUI::new_test(30, 6);
            tui.set_layout(layout);
            tui.new_server_tab("s", None);
            let chan = ChanNameRef::new("#c");
            tui.new_chan_tab("s", chan);
            tui.next_tab();
            tui.next_tab();
            tui.add_privmsg(
                "bob",
                msg,
                time::at_utc(time::Timespec::new(0, 0)),
                &MsgTarget::Chan { serv: "s", chan },
                false,
                false,
            );
            tui.draw();
            let buffer = tui.get_front_buffer();
            let cells = &buffer.content;
            let base = cells
                .iter()
                .position(|cell| cell.symbol().starts_with('◌'))
                .unwrap();
            assert_eq!(cells[base + 1].symbol(), "a");
        }
    }
}

#[test]
fn combining_mark_uses_base_character_style() {
    use ratatui::style::Modifier;

    let mut tui = TUI::new_test(30, 6);
    tui.new_server_tab("s", None);
    let chan = ChanNameRef::new("#c");
    tui.new_chan_tab("s", chan);
    tui.next_tab();
    tui.next_tab();
    tui.add_privmsg(
        "bob",
        "a\u{2}\u{301}b",
        time::at_utc(time::Timespec::new(0, 0)),
        &MsgTarget::Chan { serv: "s", chan },
        false,
        false,
    );
    tui.draw();
    let buffer = tui.get_front_buffer();
    let base = buffer
        .content
        .iter()
        .find(|cell| cell.symbol() == "a\u{301}")
        .unwrap();
    let next = buffer
        .content
        .iter()
        .find(|cell| cell.symbol() == "b" && cell.modifier.contains(Modifier::BOLD))
        .unwrap();
    assert!(!base.modifier.contains(Modifier::BOLD));
    assert!(next.modifier.contains(Modifier::BOLD));
}
