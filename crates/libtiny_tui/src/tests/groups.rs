use std::fs;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use libtiny_common::{ChanNameRef, MsgSource, MsgTarget, TabStyle};
use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier},
};
use tempfile::tempdir;

use crate::{config::Colors, test_utils::buffer_str, tui::TUI};

fn row(buf: &Buffer, y: u16) -> String {
    buffer_str(buf, buf.area.width, buf.area.height)
        .lines()
        .nth(y as usize)
        .unwrap()
        .to_owned()
}

fn cell<'a>(buf: &'a Buffer, y: u16, symbol: &str) -> &'a ratatui::buffer::Cell {
    (0..buf.area.width)
        .map(|x| &buf[(x, y)])
        .find(|cell| cell.symbol() == symbol)
        .unwrap_or_else(|| panic!("missing {symbol:?} in row {:?}", row(buf, y)))
}

fn press(tui: &mut TUI, code: KeyCode, modifiers: KeyModifiers) {
    tui.handle_input_event(Event::Key(KeyEvent::new(code, modifiers)));
}

fn target<'a>(serv: &'a str, chan: &'a str) -> MsgTarget<'a> {
    MsgTarget::Chan {
        serv,
        chan: ChanNameRef::new(chan),
    }
}

fn draw(tui: &mut TUI) -> Buffer {
    tui.draw();
    tui.get_front_buffer()
}

fn assert_source(tui: &TUI, serv: &str, name: Option<&str>) {
    match (tui.current_tab(), name) {
        (Some(MsgSource::Serv { serv: actual }), None) => assert_eq!(actual, serv),
        (Some(MsgSource::Chan { serv: actual, chan }), Some(name)) => {
            assert_eq!(actual, serv);
            assert_eq!(chan.as_ref(), ChanNameRef::new(name));
        }
        (Some(MsgSource::User { serv: actual, nick }), Some(name)) => {
            assert_eq!(actual, serv);
            assert_eq!(nick, name);
        }
        _ => panic!("unexpected current source"),
    }
}

fn assert_style(cell: &ratatui::buffer::Cell, style: crate::config::Style) {
    assert_eq!(cell.fg, style.fg);
    assert_eq!(cell.bg, style.bg);
    assert!(cell.modifier.contains(style.modifiers));
}

#[test]
fn groups_use_sources_not_aliases_and_show_only_selected_children() {
    let mut tui = TUI::new_test(100, 6);
    tui.new_server_tab("server-a", Some("Friendly A".into()));
    tui.new_chan_tab("server-a", ChanNameRef::new("#general"));
    tui.new_user_tab("server-a", "alice");
    tui.new_server_tab("server-b", Some("Friendly B".into()));
    tui.new_chan_tab("server-b", ChanNameRef::new("#chat"));
    tui.switch("#general");
    let buf = draw(&mut tui);
    let parents = row(&buf, 4);
    let children = row(&buf, 5);
    assert!(
        parents.contains("mentions")
            && parents.contains("Friendly A")
            && parents.contains("Friendly B"),
        "{parents}"
    );
    assert!(!parents.contains("server-a"), "{parents}");
    assert!(
        children.contains("#general") && children.contains("alice"),
        "{children}"
    );
    assert!(!children.contains("#chat"), "{children}");
    assert!(
        children.contains("├─ ") && children.contains("└─ "),
        "{children}"
    );
    tui.switch("server-b");
    assert_source(&tui, "server-b", None);
    let buf = draw(&mut tui);
    assert!(row(&buf, 5).contains("#chat"));
    assert!(!row(&buf, 5).contains("#general"));
    tui.switch("mentions");
    assert!(row(&draw(&mut tui), 5).trim().is_empty());
}

#[test]
fn group_markers_aggregate_without_clearing_other_children() {
    let mut tui = TUI::new_test(100, 6);
    tui.new_server_tab("a", None);
    tui.new_chan_tab("a", ChanNameRef::new("#new"));
    tui.new_chan_tab("a", ChanNameRef::new("#urgent"));
    tui.set_tab_style(TabStyle::NewMsg, &target("a", "#new"));
    tui.set_tab_style(TabStyle::Highlight, &target("a", "#urgent"));
    let colors = Colors::default();
    let buf = draw(&mut tui);
    assert_style(cell(&buf, 4, "!"), colors.tab_highlight);
    tui.switch("#urgent");
    let buf = draw(&mut tui);
    assert_style(cell(&buf, 4, "*"), colors.tab_new_msg);
    assert!(
        tui.get_tabs()
            .iter()
            .any(|t| t.visible_name == "#new" && t.style == TabStyle::NewMsg)
    );
    assert!(
        tui.get_tabs()
            .iter()
            .any(|t| t.visible_name == "#urgent" && t.style == TabStyle::Normal)
    );
    tui.switch("#new");
    assert!(!row(&draw(&mut tui), 4).contains('*'));
}

#[test]
fn hidden_parent_and_child_arrows_keep_maximum_notification_color() {
    let colors = Colors::default();
    let mut tui = TUI::new_test(18, 6);
    for serv in ["server-left", "server-middle", "server-right"] {
        tui.new_server_tab(serv, None);
        tui.new_chan_tab(serv, ChanNameRef::new("#activity"));
    }
    tui.set_tab_style(TabStyle::Highlight, &target("server-left", "#activity"));
    tui.set_tab_style(TabStyle::NewMsg, &target("server-right", "#activity"));
    tui.switch("server-middle");
    let buf = draw(&mut tui);
    assert_style(cell(&buf, 4, "<"), colors.tab_highlight);
    assert_style(cell(&buf, 4, ">"), colors.tab_new_msg);

    let mut tui = TUI::new_test(18, 6);
    tui.new_server_tab("s", None);
    for name in ["#left-long", "#middle-long", "#right-long"] {
        tui.new_chan_tab("s", ChanNameRef::new(name));
    }
    tui.set_tab_style(TabStyle::Highlight, &target("s", "#left-long"));
    tui.set_tab_style(TabStyle::NewMsg, &target("s", "#right-long"));
    tui.switch("#middle-long");
    let buf = draw(&mut tui);
    assert_style(cell(&buf, 5, "<"), colors.tab_highlight);
    assert_style(cell(&buf, 5, ">"), colors.tab_new_msg);
}

#[test]
fn narrow_server_focus_preserves_arrows_and_notification_marker() {
    let mut tui = TUI::new_test(8, 3);
    tui.new_server_tab("long-server", None);
    tui.new_chan_tab("long-server", ChanNameRef::new("#unread"));
    tui.new_server_tab("later", None);
    tui.set_tab_style(TabStyle::Highlight, &target("long-server", "#unread"));
    tui.switch("long-server");

    let buf = draw(&mut tui);
    assert_eq!(buf[(0, 1)].symbol(), "<");
    assert_eq!(buf[(7, 1)].symbol(), ">");
    assert_eq!(buf[(6, 1)].symbol(), "!");
    assert_style(&buf[(6, 1)], Colors::default().tab_highlight);
    assert_eq!(buf[(1, 1)].symbol(), "l");

    tui.set_size(3, 3);
    let buf = draw(&mut tui);
    assert_eq!(buf[(0, 1)].symbol(), "<");
    assert_eq!(buf[(1, 1)].symbol(), "l");
    assert_eq!(buf[(2, 1)].symbol(), ">");
    for (width, expected) in [(1, "l"), (2, "lo")] {
        tui.set_size(width, 3);
        let buf = draw(&mut tui);
        assert_eq!(row(&buf, 1), expected);
    }
}

#[test]
fn multicolumn_grapheme_does_not_overwrite_server_scroll_arrow() {
    let mut tui = TUI::new_test(3, 3);
    tui.new_server_tab("complex", Some("क्ष्म".into()));
    tui.new_server_tab("later", None);
    tui.switch("complex");

    let buf = draw(&mut tui);
    assert_eq!(buf[(0, 1)].symbol(), "<");
    assert_eq!(buf[(1, 1)].symbol(), "�");
    assert_eq!(buf[(2, 1)].symbol(), ">");
}

#[test]
fn server_window_prefers_complete_items_to_the_left() {
    let mut tui = TUI::new_test(13, 3);
    for server in ["a", "b", "c"] {
        tui.new_server_tab(server, None);
    }
    tui.switch("b");
    let buf = draw(&mut tui);
    let parents = row(&buf, 1);
    assert_eq!(buf[(0, 1)].symbol(), "<");
    assert_eq!(buf[(12, 1)].symbol(), ">");
    assert!(parents.contains('a') && parents.contains('b'), "{parents}");
    assert!(!parents.contains('c'), "{parents}");
}

fn selected_group_start(buf: &Buffer, y: u16, icon: &str) -> u16 {
    let style = Colors::default().tab_group_active;
    let icon_x = (1..buf.area.width)
        .find(|&x| {
            let cell = &buf[(x, y)];
            cell.symbol() == icon && cell.fg == style.fg && cell.bg == style.bg
        })
        .unwrap_or_else(|| panic!("missing active server badge in {:?}", row(buf, y)));
    assert_style(&buf[(icon_x, y)], style);
    assert_style(&buf[(icon_x - 1, y)], style);
    icon_x - 1
}

fn check_server_group_alignment(settings: &str, icon: &str) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yml");
    write_config(&path, settings);
    let mut tui = TUI::new_test_with_config(80, 6, path);
    let long = format!("{}.onion", "a".repeat(50));
    tui.new_server_tab(&long, None);
    tui.new_chan_tab(&long, ChanNameRef::new("#unread"));
    tui.set_tab_style(TabStyle::Highlight, &target(&long, "#unread"));
    tui.new_server_tab("localhost", None);
    tui.new_chan_tab("localhost", ChanNameRef::new("#i2p"));
    tui.new_chan_tab("localhost", ChanNameRef::new("#i2p-chat"));

    for (selection, channel) in [("localhost", None), ("#i2p", Some("#i2p"))] {
        tui.switch(selection);
        assert_source(&tui, "localhost", channel);
        let buf = draw(&mut tui);
        let parent_x = selected_group_start(&buf, 4, icon);
        let child_x = (0..80)
            .find(|&x| buf[(x, 5)].symbol() == "├")
            .unwrap_or_else(|| panic!("missing child tree in {:?}", row(&buf, 5)));
        assert_eq!(
            parent_x,
            child_x,
            "selection {selection}, parents {:?}, children {:?}",
            row(&buf, 4),
            row(&buf, 5)
        );
        let children = row(&buf, 5);
        assert!(
            children.contains("#i2p") && children.contains("#i2p-chat"),
            "{children}"
        );
        assert_style(cell(&buf, 4, "<"), Colors::default().tab_highlight);
        assert!(
            tui.get_tabs()
                .iter()
                .any(|tab| tab.visible_name == "#unread" && tab.style == TabStyle::Highlight)
        );
    }
}

#[test]
fn server_group_alignment_default_font() {
    check_server_group_alignment("", "●");
}

#[test]
fn server_group_alignment_nerd_font() {
    check_server_group_alignment("nerd_font: true", "");
}

#[test]
fn server_group_alignment_uses_display_columns_for_unicode_aliases() {
    let mut tui = TUI::new_test(80, 6);
    let alias = format!("{}.onion", "日本".repeat(13));
    tui.new_server_tab("unicode-host", Some(alias.clone()));
    tui.new_server_tab("localhost", None);
    tui.new_chan_tab("localhost", ChanNameRef::new("#i2p"));
    tui.new_chan_tab("localhost", ChanNameRef::new("#i2p-chat"));
    tui.switch("localhost");

    let buf = draw(&mut tui);
    let parent_x = selected_group_start(&buf, 4, "●");
    assert_eq!(parent_x, 1);
    assert_eq!(buf[(parent_x, 5)].symbol(), "├");
    assert!(!row(&buf, 4).contains("日本"));

    tui.set_size(160, 6);
    let buf = draw(&mut tui);
    let parent_x = selected_group_start(&buf, 4, "●");
    assert_eq!(parent_x, 76);
    assert!(row(&buf, 4).contains(&alias));
    assert_eq!(buf[(parent_x, 5)].symbol(), "├");
    let children = row(&buf, 5);
    assert!(
        children.contains("#i2p") && children.contains("#i2p-chat"),
        "{children}"
    );
}

#[test]
fn server_group_alignment_accepts_half_width_and_keeps_right_items() {
    let mut tui = TUI::new_test(40, 6);
    tui.new_server_tab("ab", None);
    tui.new_server_tab("focus", None);
    tui.new_chan_tab("focus", ChanNameRef::new("#x"));
    tui.new_server_tab("later", None);
    tui.switch("focus");

    let buf = draw(&mut tui);
    let parent_x = selected_group_start(&buf, 4, "●");
    assert_eq!(parent_x, 20);
    assert_eq!(buf[(parent_x, 5)].symbol(), "└");
    let parents = row(&buf, 4);
    assert!(
        parents.contains("mentions") && parents.contains("ab") && parents.contains("later"),
        "{parents}"
    );
    assert!(row(&buf, 5).contains("#x"));
}

#[test]
fn server_group_alignment_clipped_parent() {
    let mut tui = TUI::new_test(80, 6);
    let long = format!("{}.onion", "q".repeat(90));
    tui.new_server_tab(&long, None);
    tui.new_chan_tab(&long, ChanNameRef::new("#i2p"));
    tui.new_chan_tab(&long, ChanNameRef::new("#i2p-chat"));
    tui.switch(&long);

    let buf = draw(&mut tui);
    assert_eq!(buf[(0, 4)].symbol(), "<");
    assert_eq!(buf[(1, 4)].symbol(), "q");
    assert_eq!(buf[(79, 4)].symbol(), "q");
    assert_style(&buf[(1, 4)], Colors::default().tab_active);
    assert_eq!(buf[(1, 5)].symbol(), "├");
    let children = row(&buf, 5);
    assert!(
        children.contains("#i2p") && children.contains("#i2p-chat"),
        "{children}"
    );
}

#[test]
fn server_group_alignment_hidden_child_and_resize_preserve_state() {
    let mut tui = TUI::new_test(32, 6);
    tui.new_server_tab("localhost", None);
    tui.new_chan_tab("localhost", ChanNameRef::new("#unread-child"));
    tui.new_chan_tab("localhost", ChanNameRef::new("#i2p-chat"));
    tui.set_tab_style(TabStyle::Highlight, &target("localhost", "#unread-child"));
    tui.switch("#i2p-chat");
    for c in "draft".chars() {
        press(&mut tui, KeyCode::Char(c), KeyModifiers::NONE);
    }

    let buf = draw(&mut tui);
    let parent_x = selected_group_start(&buf, 4, "●");
    assert_eq!(buf[(parent_x, 5)].symbol(), "<");
    assert_style(&buf[(parent_x, 5)], Colors::default().tab_highlight);
    assert_eq!(buf[(parent_x + 1, 5)].symbol(), "└");
    let children = row(&buf, 5);
    assert!(
        children.contains("#i2p-chat") && !children.contains("#unread-child"),
        "{children}"
    );

    for width in [0, 1, 2, 3, 4, 12, 40, 80] {
        for height in 0..=3 {
            tui.set_size(width, height);
            tui.draw();
            assert_source(&tui, "localhost", Some("#i2p-chat"));
            assert!(
                tui.get_tabs()
                    .iter()
                    .any(|tab| tab.visible_name == "#unread-child"
                        && tab.style == TabStyle::Highlight)
            );
        }
    }

    tui.set_size(80, 6);
    let buf = draw(&mut tui);
    assert!(buffer_str(&buf, 80, 6).contains("draft"));
    let parent_x = selected_group_start(&buf, 4, "●");
    assert_eq!(buf[(parent_x, 5)].symbol(), "├");
    assert_style(cell(&buf, 5, "#"), Colors::default().tab_highlight);
    assert!(row(&buf, 5).contains("#i2p-chat"));
}

#[test]
fn selection_survives_insertion_removal_navigation_and_reordering() {
    let mut tui = TUI::new_test(100, 6);
    tui.new_server_tab("a", None);
    tui.new_chan_tab("a", ChanNameRef::new("#first"));
    tui.new_server_tab("b", None);
    tui.new_chan_tab("b", ChanNameRef::new("#last"));
    tui.switch("#last");
    tui.set_tab_style(TabStyle::NewMsg, &target("a", "#first"));
    tui.new_user_tab("a", "alice");
    assert_source(&tui, "b", Some("#last"));
    assert!(
        tui.get_tabs()
            .iter()
            .any(|t| t.visible_name == "#first" && t.style == TabStyle::NewMsg)
    );
    tui.close_user_tab("a", "alice");
    assert_source(&tui, "b", Some("#last"));
    assert!(
        tui.get_tabs()
            .iter()
            .any(|t| t.visible_name == "#first" && t.style == TabStyle::NewMsg)
    );
    press(&mut tui, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert_source(&tui, "b", None);
    press(&mut tui, KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert_source(&tui, "b", Some("#last"));
    press(&mut tui, KeyCode::Char('1'), KeyModifiers::ALT);
    assert_source(&tui, "mentions", None);
    let shortcut = tui
        .get_tabs()
        .iter()
        .find(|t| t.visible_name == "#last")
        .unwrap()
        .switch
        .unwrap();
    press(&mut tui, KeyCode::Char(shortcut), KeyModifiers::ALT);
    assert_source(&tui, "b", Some("#last"));
    press(&mut tui, KeyCode::Left, KeyModifiers::ALT);
    assert_source(&tui, "b", Some("#last"));
    tui.switch("a");
    press(&mut tui, KeyCode::Right, KeyModifiers::ALT);
    assert_source(&tui, "a", None);
    let tabs = tui.get_tabs();
    let a = tabs.iter().position(|t| t.visible_name == "a").unwrap();
    assert_eq!(tabs[a + 1].visible_name, "#first");
    tui.try_handle_cmd("switch #last", &MsgSource::Serv { serv: "a".into() });
    assert_source(&tui, "b", Some("#last"));
    tui.close_chan_tab("b", ChanNameRef::new("#last"));
    assert_source(&tui, "b", None);
    tui.close_server_tab("b");
    assert_ne!(tui.current_tab().unwrap().serv_name(), "b");
    tui.close_server_tab("a");
    tui.close_server_tab("mentions");
    assert!(tui.get_tabs().is_empty());
    tui.draw();
    press(&mut tui, KeyCode::Char('n'), KeyModifiers::CONTROL);
    press(&mut tui, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(
        tui.handle_input_event(Event::Paste("multi\nline".into()))
            .is_none()
    );
    tui.new_server_tab("again", None);
    assert_source(&tui, "again", None);
}

#[test]
fn child_moves_and_group_deletion_preserve_source_and_predecessor() {
    let mut tui = TUI::new_test(80, 6);
    tui.new_server_tab("a", None);
    tui.new_chan_tab("a", ChanNameRef::new("#one"));
    tui.new_chan_tab("a", ChanNameRef::new("#two"));
    tui.new_server_tab("b", None);
    tui.new_chan_tab("b", ChanNameRef::new("#three"));
    tui.switch("#two");
    press(&mut tui, KeyCode::Left, KeyModifiers::ALT);
    assert_source(&tui, "a", Some("#two"));
    let tabs = tui.get_tabs();
    let two = tabs.iter().position(|t| t.visible_name == "#two").unwrap();
    assert_eq!(tabs[two + 1].visible_name, "#one");
    press(&mut tui, KeyCode::Right, KeyModifiers::ALT);
    assert_source(&tui, "a", Some("#two"));
    tui.switch("#three");
    tui.close_server_tab("a");
    assert_source(&tui, "b", Some("#three"));
    tui.close_server_tab("b");
    assert_source(&tui, "mentions", None);
}

#[test]
fn join_part_marker_uses_existing_notification_style() {
    let mut tui = TUI::new_test(60, 6);
    tui.new_server_tab("server", None);
    tui.new_chan_tab("server", ChanNameRef::new("#chan"));
    tui.set_tab_style(TabStyle::JoinOrPart, &target("server", "#chan"));
    let buf = draw(&mut tui);
    assert_style(cell(&buf, 4, "+"), Colors::default().tab_joinpart);
}

#[test]
fn narrow_and_zero_sized_resize_preserves_input_and_selection() {
    let mut tui = TUI::new_test(40, 6);
    tui.new_server_tab("長い日本語e\u{301}server", None);
    tui.new_chan_tab(
        "長い日本語e\u{301}server",
        ChanNameRef::new("#日本語e\u{301}channel"),
    );
    tui.switch("#日本語e\u{301}channel");
    for c in "draft".chars() {
        press(&mut tui, KeyCode::Char(c), KeyModifiers::NONE);
    }
    for width in [0, 1, 2, 3, 7, 15, 40] {
        for height in 0..=3 {
            tui.set_size(width, height);
            tui.draw();
            assert_source(
                &tui,
                "長い日本語e\u{301}server",
                Some("#日本語e\u{301}channel"),
            );
            if width > 0 && height == 2 {
                let buf = tui.get_front_buffer();
                assert!(!row(&buf, 1).contains('<') && !row(&buf, 1).contains('>'));
            }
        }
    }
    tui.set_size(40, 6);
    assert!(buffer_str(&draw(&mut tui), 40, 6).contains("draft"));
}

fn write_config(path: &std::path::Path, settings: &str) {
    fs::write(path, format!("servers: []\ndefaults: {{}}\n{settings}\n")).unwrap();
}

fn reload(tui: &mut TUI, path: &std::path::Path, settings: &str) {
    write_config(path, settings);
    tui.try_handle_cmd(
        "reload",
        &MsgSource::Serv {
            serv: "mentions".into(),
        },
    );
}

#[test]
fn nerd_font_defaults_and_reload_preserve_last_valid_configuration() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yml");
    write_config(&path, "");
    let mut tui = TUI::new_test_with_config(100, 6, path.clone());
    tui.new_server_tab("server", None);
    tui.new_chan_tab("server", ChanNameRef::new("#chan"));
    tui.new_user_tab("server", "alice");
    tui.switch("server");
    let plain = buffer_str(&draw(&mut tui), 100, 6);
    assert!(plain.contains('●') && plain.contains('·') && plain.contains('@'));
    assert!(!plain.contains(''));
    reload(&mut tui, &path, "nerd_font: false");
    assert!(buffer_str(&draw(&mut tui), 100, 6).contains('●'));
    reload(&mut tui, &path, "nerd_font: true");
    let nerd = buffer_str(&draw(&mut tui), 100, 6);
    for glyph in ['', '', '', ''] {
        assert!(nerd.contains(glyph), "missing {glyph}: {nerd}");
    }
    reload(&mut tui, &path, "nerd_font: invalid");
    assert!(buffer_str(&draw(&mut tui), 100, 6).contains(''));
    reload(&mut tui, &path, "nerd_font: [");
    assert!(buffer_str(&draw(&mut tui), 100, 6).contains(''));
    reload(&mut tui, &path, "nerd_font: false");
    let plain = buffer_str(&draw(&mut tui), 100, 6);
    assert!(plain.contains('●') && !plain.contains('') && !plain.contains(''));
}

#[test]
fn customized_badges_labels_markers_and_powerline_colors_are_independent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yml");
    let settings = "nerd_font: true\ncolors:\n  tab_highlight: { fg: 101, bg: 102, attrs: [italic] }\n  tab_group_active: { fg: 103, bg: 104, attrs: [bold] }";
    write_config(&path, settings);
    let mut tui = TUI::new_test_with_config(100, 6, path.clone());
    tui.new_server_tab("server", None);
    tui.new_chan_tab("server", ChanNameRef::new("#alert"));
    tui.set_tab_style(TabStyle::Highlight, &target("server", "#alert"));
    tui.switch("server");
    let buf = draw(&mut tui);
    let badge = cell(&buf, 4, "");
    // There are two server badges; locate the selected badge by its configured colors.
    let badge = (0..100)
        .map(|x| &buf[(x, 4)])
        .find(|c| c.symbol() == "" && c.bg == Color::Indexed(104))
        .unwrap_or(badge);
    assert_eq!(
        (badge.fg, badge.bg),
        (Color::Indexed(103), Color::Indexed(104))
    );
    assert!(badge.modifier.contains(Modifier::BOLD));
    let marker = cell(&buf, 4, "!");
    assert_eq!(
        (marker.fg, marker.bg),
        (Color::Indexed(101), Color::Indexed(102))
    );
    assert!(marker.modifier.contains(Modifier::ITALIC));
    let label = cell(&buf, 5, "#");
    assert_eq!(
        (label.fg, label.bg),
        (Color::Indexed(101), Color::Indexed(102))
    );
    let separator = (0..100)
        .map(|x| &buf[(x, 5)])
        .find(|c| c.symbol() == "")
        .unwrap();
    assert_eq!(
        (separator.fg, separator.bg),
        (Color::Indexed(104), Color::Indexed(102))
    );
    reload(
        &mut tui,
        &path,
        "nerd_font: true\ncolors:\n  tab_group_active: { fg: 103, bg: default }",
    );
    let buf = draw(&mut tui);
    assert!(!row(&buf, 5).contains(''));
}
