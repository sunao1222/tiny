use std::panic::Location;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use libtiny_common::{ChanNameRef, MsgTarget};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

use crate::test_utils::{buffer_str, expect_screen};
use crate::tui::{TUI, TUIRet};

mod events;
mod groups;
mod layout;
mod reload_history;
mod resize;
mod unicode;

mod config;

fn enter_string(tui: &mut TUI, s: &str) {
    for c in s.chars() {
        tui.handle_input_event(Event::Key(key(KeyCode::Char(c))));
    }
}

fn completion_input(tui: &mut TUI) -> String {
    tui.draw();
    let screen = buffer_str(&tui.get_front_buffer(), 80, 7);
    screen.lines().nth(3).unwrap().trim_end().to_owned()
}

fn press(tui: &mut TUI, key: KeyEvent) {
    tui.handle_input_event(Event::Key(key));
}

fn submit(tui: &mut TUI) -> String {
    match tui.handle_input_event(Event::Key(key(KeyCode::Enter))) {
        Some(TUIRet::Input { msg, .. }) => msg,
        _ => panic!("expected submitted input"),
    }
}

#[test]
fn uppercase_key_bindings_match_terminal_input() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yml");
    std::fs::write(
        &config,
        "servers: []\ndefaults: {}\nkey_map:\n  A:\n    command: uppercase\n  alt_Z:\n    command: alt-uppercase\n",
    )
    .unwrap();
    let mut tui = TUI::new_test_with_config(80, 5, config);

    for (character, modifiers, expected) in [
        ('A', KeyModifiers::SHIFT, "uppercase"),
        (
            'Z',
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            "alt-uppercase",
        ),
    ] {
        match tui.handle_input_event(Event::Key(KeyEvent::new(
            KeyCode::Char(character),
            modifiers,
        ))) {
            Some(TUIRet::KeyCommand { cmd, .. }) => assert_eq!(cmd, expected),
            result => panic!("expected command {expected}, got {result:?}"),
        }
    }
}

#[test]
fn command_completion_cycles_and_commits_edits() {
    let mut tui = TUI::new_test(80, 7);
    tui.add_command_completion("close");
    tui.add_command_completion("connect");
    tui.add_command_completion("close");

    enter_string(&mut tui, "/");
    for expected in [
        "/clear", "/close", "/connect", "/ignore", "/notify", "/quit", "/reload", "/switch",
        "/clear",
    ] {
        press(&mut tui, key(KeyCode::Tab));
        assert_eq!(completion_input(&mut tui), expected);
    }
    press(&mut tui, ctrl('a'));
    press(&mut tui, ctrl('k'));

    enter_string(&mut tui, "/c");
    press(&mut tui, key(KeyCode::Tab));
    assert_eq!(completion_input(&mut tui), "/clear");
    press(&mut tui, key(KeyCode::Tab));
    assert_eq!(completion_input(&mut tui), "/close");
    press(&mut tui, key(KeyCode::Up));
    assert_eq!(completion_input(&mut tui), "/connect");
    press(&mut tui, key(KeyCode::Tab));
    assert_eq!(completion_input(&mut tui), "/clear");
    press(&mut tui, key(KeyCode::Down));
    assert_eq!(completion_input(&mut tui), "/connect");
    press(&mut tui, key(KeyCode::Down));
    assert_eq!(completion_input(&mut tui), "/close");

    press(&mut tui, key(KeyCode::Char('!')));
    assert_eq!(completion_input(&mut tui), "/close!");
    assert_eq!(submit(&mut tui), "/close!");
    press(&mut tui, key(KeyCode::Up));
    assert_eq!(completion_input(&mut tui), "/close!");
}

#[test]
fn command_completion_is_shared_with_existing_and_new_tabs() {
    let mut tui = TUI::new_test(80, 7);
    tui.new_server_tab("existing", None);
    tui.add_command_completion("join");

    enter_string(&mut tui, "/jo");
    press(&mut tui, key(KeyCode::Tab));
    assert_eq!(completion_input(&mut tui), "/join");
    assert_eq!(submit(&mut tui), "/join");

    tui.next_tab();
    enter_string(&mut tui, "/jo");
    press(&mut tui, key(KeyCode::Tab));
    assert_eq!(completion_input(&mut tui), "/join");
    assert_eq!(submit(&mut tui), "/join");

    tui.new_server_tab("new", None);
    tui.next_tab();
    enter_string(&mut tui, "/jo");
    press(&mut tui, key(KeyCode::Tab));
    assert_eq!(completion_input(&mut tui), "/join");
    assert_eq!(submit(&mut tui), "/join");
}

#[test]
fn completion_respects_command_boundaries_and_nick_contexts() {
    let mut tui = TUI::new_test(80, 7);
    let serv = "server";
    let chan = ChanNameRef::new("#chan");
    tui.new_server_tab(serv, None);
    tui.new_chan_tab(serv, chan);
    tui.next_tab();
    tui.next_tab();
    let target = MsgTarget::Chan { serv, chan };
    tui.add_nick("Jack", None, &target);
    tui.add_nick("jane", None, &target);
    tui.add_nick("nina", None, &target);
    tui.add_command_completion("join");

    for (text, cursor, expected) in [
        ("/jn café λ", Some(2), "/join café λ"),
        ("/jo\u{2003}café", Some(3), "/join\u{2003}café"),
        ("/jz", Some(2), "/jz"),
        ("/ja", None, "/ja"),
        ("/J", None, "/J"),
        ("/jo", Some(0), "Jack/jo"),
        ("/msg ni", None, "/msg nina"),
        ("/msg /ni", None, "/msg /nina"),
        ("hello ni", None, "hello nina"),
        ("hello /ni", None, "hello /nina"),
    ] {
        enter_string(&mut tui, text);
        if let Some(cursor) = cursor {
            press(&mut tui, ctrl('a'));
            for _ in 0..cursor {
                press(&mut tui, key(KeyCode::Right));
            }
        }
        press(&mut tui, key(KeyCode::Tab));
        assert_eq!(completion_input(&mut tui), expected, "input: {text:?}");
        assert_eq!(submit(&mut tui), expected, "input: {text:?}");
    }
}

#[test]
fn exit_dialogue_blocks_completion() {
    let mut tui = TUI::new_test(80, 7);
    tui.add_command_completion("join");
    enter_string(&mut tui, "/jo");
    press(&mut tui, ctrl('c'));
    press(&mut tui, key(KeyCode::Tab));
    assert!(completion_input(&mut tui).starts_with("Really quit?"));
    press(&mut tui, key(KeyCode::Esc));
    assert_eq!(completion_input(&mut tui), "/jo");
    press(&mut tui, key(KeyCode::Tab));
    assert_eq!(completion_input(&mut tui), "/join");
}

#[test]
fn init_screen() {
    let mut tui = TUI::new_test(20, 6);
    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|Any mentions to you |
         |will be listed here.|
         |                    |";
    expect_screen(screen, &tui.get_front_buffer(), 20, 3, Location::caller());
}

#[test]
fn close_rightmost_tab() {
    // Closing the selected server selects the preceding remaining tab.
    let mut tui = TUI::new_test(20, 6);
    tui.new_server_tab("irc.server_1.org", None);
    tui.new_server_tab("irc.server_2.org", None);
    tui.next_tab();
    tui.next_tab();
    tui.draw();
    assert_eq!(tui.get_tabs()[2].visible_name(), "irc.server_2.org");

    #[rustfmt::skip]
    let screen =
        "|                    |
         |                    |
         |                    |";
    expect_screen(screen, &tui.get_front_buffer(), 20, 3, Location::caller());

    tui.close_server_tab("irc.server_2.org");
    tui.draw();
    assert!(buffer_str(&tui.get_front_buffer(), 20, 6).contains("irc.server_1.org"));

    #[rustfmt::skip]
    let screen =
        "|                    |
         |                    |
         |                    |";
    expect_screen(screen, &tui.get_front_buffer(), 20, 3, Location::caller());

    tui.close_server_tab("irc.server_1.org");
    tui.draw();
    assert!(buffer_str(&tui.get_front_buffer(), 20, 6).contains("mentions"));

    #[rustfmt::skip]
    let screen =
        "|Any mentions to you |
         |will be listed here.|
         |                    |";
    expect_screen(screen, &tui.get_front_buffer(), 20, 3, Location::caller());
}

#[test]
fn small_screen_1() {
    let mut tui = TUI::new_test(21, 5);
    let serv = "irc.server_1.org";
    let chan = ChanNameRef::new("#chan");
    tui.new_server_tab(serv, None);
    tui.set_nick(serv, "osa1");
    tui.new_chan_tab(serv, chan);
    tui.next_tab();
    tui.next_tab();

    let target = MsgTarget::Chan { serv, chan };
    let ts = time::at_utc(time::Timespec::new(0, 0));
    tui.add_nick("123456", Some(ts), &target);
    tui.add_nick("abcdef", Some(ts), &target);

    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|00:00 +123456 +abcdef|
         |osa1:                |";

    expect_screen(screen, &tui.get_front_buffer(), 21, 2, Location::caller());

    tui.set_size(24, 5);
    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|00:00 +123456 +abcdef   |
         |osa1:                   |";

    expect_screen(screen, &tui.get_front_buffer(), 24, 2, Location::caller());

    tui.set_size(31, 5);
    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|00:00 +123456 +abcdef          |
         |osa1:                          |";

    expect_screen(screen, &tui.get_front_buffer(), 31, 2, Location::caller());
}

#[test]
fn small_screen_2() {
    let mut tui = TUI::new_test(21, 6);
    let serv = "irc.server_1.org";
    let chan = ChanNameRef::new("#chan");
    tui.new_server_tab(serv, None);
    tui.set_nick(serv, "osa1");
    tui.new_chan_tab(serv, chan);
    tui.next_tab();
    tui.next_tab();

    let target = MsgTarget::Chan { serv, chan };
    let ts = time::at_utc(time::Timespec::new(0, 0));
    tui.set_topic("Blah blah blah-", ts, serv, chan);

    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|                     |
         |00:00 Blah blah blah-|
         |osa1:                |";
    expect_screen(screen, &tui.get_front_buffer(), 21, 3, Location::caller());

    tui.add_nick("123456", Some(ts), &target);
    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|00:00 Blah blah blah-|
         |+123456              |
         |osa1:                |";
    expect_screen(screen, &tui.get_front_buffer(), 21, 3, Location::caller());
}

#[test]
fn ctrl_w() {
    let mut tui = TUI::new_test(30, 5);
    let serv = "irc.server_1.org";
    let chan = ChanNameRef::new("#chan");
    tui.new_server_tab(serv, None);
    tui.set_nick(serv, "osa1");
    tui.new_chan_tab(serv, chan);
    tui.next_tab();
    tui.next_tab();

    enter_string(&mut tui, "alskdfj asldkf asldkf aslkdfj aslkdfj asf");

    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|                              |
         |osa1: dkf aslkdfj aslkdfj asf |";
    expect_screen(screen, &tui.get_front_buffer(), 30, 2, Location::caller());

    tui.handle_input_event(Event::Key(ctrl('w')));
    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|                              |
         |osa1: asldkf aslkdfj aslkdfj  |";

    expect_screen(screen, &tui.get_front_buffer(), 30, 2, Location::caller());

    println!("~~~~~~~~~~~~~~~~~~~~~~");
    tui.handle_input_event(Event::Key(ctrl('w')));
    println!("~~~~~~~~~~~~~~~~~~~~~~");
    tui.draw();

    /*
    The buggy behavior was as below:

    let screen =
        "|                              |
         |osa1:  asldkf aslkdfj         |";
    */

    #[rustfmt::skip]
    let screen =
        "|                              |
         |osa1:  asldkf asldkf aslkdfj  |";

    expect_screen(screen, &tui.get_front_buffer(), 30, 2, Location::caller());

    tui.handle_input_event(Event::Key(ctrl('w')));
    tui.draw();

    #[rustfmt::skip]
    let screen =
        "|                              |
         |osa1: alskdfj asldkf asldkf   |";

    expect_screen(screen, &tui.get_front_buffer(), 30, 2, Location::caller());
}

// Tests text field wrapping (text_field_wrap setting)
#[test]
fn test_text_field_wrap() {
    // Screen should be wide enough to enable wrapping. See SCROLL_FALLBACK_WIDTH in text_field.rs
    let mut tui = TUI::new_test(40, 10);

    let server = "chat.freenode.net";
    tui.new_server_tab(server, None);
    tui.set_nick(server, "x");

    // Switch to server tab
    tui.next_tab();

    // Write some stuff
    let target = MsgTarget::CurrentTab;
    let ts = time::empty_tm();
    tui.add_msg("test test test", ts, &target);

    for _ in 0..37 {
        let event = Event::Key(key(KeyCode::Char('a')));
        tui.handle_input_event(event);
    }
    for _ in 0..5 {
        let event = Event::Key(key(KeyCode::Char('b')));
        tui.handle_input_event(event);
    }

    tui.draw();

    #[rustfmt::skip]
    let screen =
    "|                                        |
     |                                        |
     |                                        |
     |                                        |
     |00:00 test test test                    |
     |x: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa|
     |bbbbb                                   |";

    expect_screen(screen, &tui.get_front_buffer(), 40, 7, Location::caller());

    // Test resizing
    tui.set_size(46, 10);
    tui.draw();

    #[rustfmt::skip]
    let screen =
    "|                                              |
     |                                              |
     |                                              |
     |                                              |
     |                                              |
     |00:00 test test test                          |
     |x: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaabbbbb |";

    expect_screen(screen, &tui.get_front_buffer(), 46, 7, Location::caller());

    // Reset size
    tui.set_size(40, 10);

    // If we remove a few characters now the line above the text field should still be right above
    // the text field
    for _ in 0..6 {
        let event = Event::Key(key(KeyCode::Backspace));
        tui.handle_input_event(event);
    }

    tui.draw();

    #[rustfmt::skip]
    let screen =
    "|                                        |
     |                                        |
     |                                        |
     |                                        |
     |                                        |
     |00:00 test test test                    |
     |x: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa |";

    expect_screen(screen, &tui.get_front_buffer(), 40, 7, Location::caller());

    // On making screen smaller we should fall back to scrolling
    tui.set_size(30, 10);
    for _ in 0..5 {
        let event = Event::Key(key(KeyCode::Char('b')));
        tui.handle_input_event(event);
    }
    tui.draw();

    #[rustfmt::skip]
    let screen =
    "|                              |
     |                              |
     |                              |
     |                              |
     |                              |
     |00:00 test test test          |
     |x: aaaaaaaaaaaaaaaaaaaaabbbbb |";

    expect_screen(screen, &tui.get_front_buffer(), 30, 7, Location::caller());

    tui.set_size(40, 10);
    tui.draw();

    #[rustfmt::skip]
    let screen =
    "|                                        |
     |                                        |
     |                                        |
     |                                        |
     |00:00 test test test                    |
     |x: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab|
     |bbbb                                    |";

    expect_screen(screen, &tui.get_front_buffer(), 40, 7, Location::caller());

    // Wrapping on words - splits lines on whitespace
    for _ in 0..6 {
        let event = Event::Key(key(KeyCode::Backspace));
        tui.handle_input_event(event);
    }
    // InputLine cache gets invalidated after backspace, need to redraw to calculate.
    tui.draw();
    let event = Event::Key(key(KeyCode::Char(' ')));
    tui.handle_input_event(event);

    for _ in 0..5 {
        let event = Event::Key(key(KeyCode::Char('b')));
        tui.handle_input_event(event);
    }

    tui.draw();

    #[rustfmt::skip]
    let screen =
    "|                                        |
     |                                        |
     |                                        |
     |                                        |
     |00:00 test test test                    |
     |x: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  |
     |bbbbb                                   |";

    expect_screen(screen, &tui.get_front_buffer(), 40, 7, Location::caller());

    // TODO: Test changing nick (osa: I don't understand how nick length is taken into account when
    // falling back to scrolling)
}

#[test]
fn inactive_tab_reflects_message_and_highlight_colors() {
    use libtiny_common::TabStyle;

    let mut tui = TUI::new_test(40, 6);
    let chan = ChanNameRef::new("#demo");
    let target = MsgTarget::Chan { serv: "s", chan };
    tui.new_server_tab("s", None);
    tui.new_chan_tab("s", chan);
    tui.next_tab(); // Select the server so its inactive channel remains visible.
    let colors = crate::config::Colors::default();
    for (style, expected) in [
        (TabStyle::NewMsg, colors.tab_new_msg),
        (TabStyle::Highlight, colors.tab_highlight),
    ] {
        tui.set_tab_style(style, &target);
        tui.draw();
        let buffer = tui.get_front_buffer();
        let tab = (0..40).find(|&x| buffer[(x, 5)].symbol() == "#").unwrap();
        let cell = &buffer[(tab, 5)];
        assert_eq!(cell.fg, expected.fg);
        assert_eq!(cell.bg, expected.bg);
        assert!(cell.modifier.contains(expected.modifiers));
    }
}
