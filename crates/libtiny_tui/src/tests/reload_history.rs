use std::fs;

use libtiny_common::{ChanNameRef, MsgSource, MsgTarget};
use tempfile::tempdir;
use term_input::{Event as InputEvent, Key};

use crate::test_utils::buffer_str;
use crate::tui::TUI;

fn reload(tui: &mut TUI, config: &std::path::Path, settings: &str) {
    tui.switch("mentions");
    fs::write(config, format!("servers: []\ndefaults: {{}}\n{settings}\n")).unwrap();
    tui.try_handle_cmd(
        "reload",
        &MsgSource::Serv {
            serv: "mentions".into(),
        },
    );
}

fn screen(tui: &mut TUI, width: u16, height: u16) -> String {
    tui.draw();
    buffer_str(&tui.get_front_buffer(), width, height)
}

fn press(tui: &mut TUI, key: Key) {
    tui.handle_input_event(InputEvent::Key(key), &mut None);
}

#[test]
fn initial_mentions_obey_configured_scrollback() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yml");
    fs::write(&config, "servers: []\ndefaults: {}\nscrollback: 1\n").unwrap();
    let mut tui = TUI::new_test_with_config(60, 8, config);
    tui.add_client_msg("old mention", &MsgTarget::CurrentTab);
    tui.add_client_msg("latest mention", &MsgTarget::CurrentTab);
    tui.draw();

    let screen = buffer_str(&tui.get_front_buffer(), 60, 8);
    assert!(screen.contains("latest mention"), "{screen}");
    assert!(!screen.contains("old mention"), "{screen}");
    assert!(!screen.contains("Any mentions"), "{screen}");
}

#[test]
fn reload_relayouts_existing_history_without_changing_timestamp_grouping() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yml");
    fs::write(&config, "servers: []\ndefaults: {}\nlayout: compact\n").unwrap();
    let mut tui = TUI::new_test_with_config(60, 8, config.clone());
    let chan = ChanNameRef::new("#demo");
    let target = MsgTarget::Chan {
        serv: "server",
        chan,
    };
    let ts = time::at_utc(time::Timespec::new(0, 0));
    tui.new_server_tab("server", None);
    tui.new_chan_tab("server", chan);
    tui.add_privmsg("long-nickname", "hello", ts, &target, false, false);
    tui.add_privmsg("bob", "second", ts, &target, false, false);
    tui.add_nick("alice", Some(ts), &target);
    tui.remove_nick("bob", Some(ts), &target);
    tui.rename_nick("old", "new", ts, &target);
    tui.switch("#demo");

    let compact = screen(&mut tui, 60, 8);
    assert!(compact.contains("00:00 long-nickname: hello"), "{compact}");
    assert!(compact.contains("bob: second"), "{compact}");
    assert!(compact.contains("+alice -bob old>new"), "{compact}");

    reload(&mut tui, &config, "layout: aligned\nmax_nick_length: 4");
    tui.switch("#demo");
    let aligned = screen(&mut tui, 60, 8);
    assert!(aligned.contains("00:00 lon…: hello"), "{aligned}");
    assert!(aligned.contains("       bob: second"), "{aligned}");
    assert!(
        aligned.contains("            +alice -bob old>new"),
        "{aligned}"
    );
    assert_eq!(aligned.matches("00:00").count(), 1, "{aligned}");

    // Omitting layout must keep the previous nick limit as well as alignment.
    reload(&mut tui, &config, "max_nick_length: 12");
    tui.switch("#demo");
    let omitted = screen(&mut tui, 60, 8);
    assert!(omitted.contains("00:00 lon…: hello"), "{omitted}");

    // A failed reload must retain the previous configuration.
    reload(&mut tui, &config, "layout: [");
    tui.switch("#demo");
    let invalid = screen(&mut tui, 60, 8);
    assert!(invalid.contains("00:00 lon…: hello"), "{invalid}");

    reload(&mut tui, &config, "layout: compact");
    tui.switch("#demo");
    let compact_again = screen(&mut tui, 60, 8);
    assert!(
        compact_again.contains("00:00 long-nickname: hello"),
        "{compact_again}"
    );
    assert!(
        compact_again.contains("+alice -bob old>new"),
        "{compact_again}"
    );
    assert_eq!(compact_again.matches("00:00").count(), 1, "{compact_again}");
}

#[test]
fn reloaded_scrollback_applies_to_existing_and_new_tabs() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yml");
    fs::write(&config, "servers: []\ndefaults: {}\nscrollback: 20\n").unwrap();
    let mut tui = TUI::new_test_with_config(60, 8, config.clone());
    let old_chan = ChanNameRef::new("#old");
    tui.new_server_tab("old-server", None);
    tui.new_chan_tab("old-server", old_chan);

    reload(&mut tui, &config, "scrollback: 0");
    tui.new_server_tab("new-server", None);
    let new_chan = ChanNameRef::new("#new");
    tui.new_chan_tab("new-server", new_chan);

    for (name, target) in [
        ("old-server", MsgTarget::Server { serv: "old-server" }),
        (
            "#old",
            MsgTarget::Chan {
                serv: "old-server",
                chan: old_chan,
            },
        ),
        ("new-server", MsgTarget::Server { serv: "new-server" }),
        (
            "#new",
            MsgTarget::Chan {
                serv: "new-server",
                chan: new_chan,
            },
        ),
    ] {
        tui.add_client_msg("discard this", &target);
        tui.add_client_msg("retain this", &target);
        tui.switch(name);
        let text = screen(&mut tui, 60, 8);
        assert!(text.contains("retain this"), "{name}: {text}");
        assert!(!text.contains("discard this"), "{name}: {text}");
    }
}

#[test]
fn scrollback_shrink_preserves_or_clamps_top_visible_logical_line() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yml");
    fs::write(&config, "servers: []\ndefaults: {}\nscrollback: 20\n").unwrap();
    let mut tui = TUI::new_test_with_config(24, 5, config.clone());
    tui.new_server_tab("server", None);
    tui.switch("server");
    for n in 0..10 {
        tui.add_client_msg(&format!("row-{n}"), &MsgTarget::CurrentTab);
    }
    for _ in 0..4 {
        press(&mut tui, Key::MouseWheelUp);
    }
    let before = screen(&mut tui, 24, 5);
    assert!(
        before.lines().next().unwrap().starts_with("row-3"),
        "{before}"
    );

    reload(&mut tui, &config, "scrollback: 8");
    tui.switch("server");
    let retained = screen(&mut tui, 24, 5);
    assert!(
        retained.lines().next().unwrap().starts_with("row-3"),
        "{retained}"
    );

    reload(&mut tui, &config, "scrollback: 5");
    tui.switch("server");
    let clamped = screen(&mut tui, 24, 5);
    assert!(
        clamped.lines().next().unwrap().starts_with("row-5"),
        "{clamped}"
    );
}

#[test]
fn activity_after_trim_does_not_append_to_an_unrelated_line() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yml");
    fs::write(&config, "servers: []\ndefaults: {}\nscrollback: 1\n").unwrap();
    let mut tui = TUI::new_test_with_config(40, 5, config);
    let chan = ChanNameRef::new("#demo");
    let target = MsgTarget::Chan {
        serv: "server",
        chan,
    };
    let ts = time::at_utc(time::Timespec::new(0, 0));
    tui.new_server_tab("server", None);
    tui.new_chan_tab("server", chan);
    tui.add_nick("first", Some(ts), &target);
    tui.add_client_msg("interruption", &target);
    tui.add_nick("second", Some(ts), &target);
    tui.switch("#demo");
    let text = screen(&mut tui, 40, 5);
    assert!(text.contains("+second"), "{text}");
    assert!(!text.contains("interruption"), "{text}");
    assert!(!text.contains("+first"), "{text}");
    assert!(!text.contains("00:00"), "{text}");
}

#[test]
fn shrinking_scrollback_keeps_the_latest_rows_visible_at_bottom() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yml");
    fs::write(&config, "servers: []\ndefaults: {}\nscrollback: 20\n").unwrap();
    let mut tui = TUI::new_test_with_config(24, 5, config.clone());
    tui.new_server_tab("server", None);
    tui.switch("server");
    for n in 0..10 {
        tui.add_client_msg(&format!("row-{n}"), &MsgTarget::CurrentTab);
    }

    reload(&mut tui, &config, "scrollback: 5");
    tui.switch("server");
    let after_shrink = screen(&mut tui, 24, 5);
    assert!(
        after_shrink.lines().next().unwrap().starts_with("row-7"),
        "{after_shrink}"
    );
    assert!(after_shrink.contains("row-9"), "{after_shrink}");

    tui.add_client_msg("row-10", &MsgTarget::CurrentTab);
    let after_insert = screen(&mut tui, 24, 5);
    assert!(
        after_insert.lines().next().unwrap().starts_with("row-8"),
        "{after_insert}"
    );
    assert!(after_insert.contains("row-10"), "{after_insert}");
}

#[test]
fn activity_growth_updates_scrollable_height() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yml");
    fs::write(&config, "servers: []\ndefaults: {}\n").unwrap();
    let mut tui = TUI::new_test_with_config(16, 4, config);
    let chan = ChanNameRef::new("#demo");
    let target = MsgTarget::Chan {
        serv: "server",
        chan,
    };
    let ts = time::at_utc(time::Timespec::new(0, 0));
    tui.new_server_tab("server", None);
    tui.new_chan_tab("server", chan);
    tui.switch("#demo");
    tui.add_nick("first", Some(ts), &target);
    screen(&mut tui, 16, 4);
    for nick in ["second", "third", "fourth", "fifth"] {
        tui.add_nick(nick, Some(ts), &target);
    }
    press(&mut tui, Key::Home);
    let top = screen(&mut tui, 16, 4);
    assert!(top.contains("00:00 +first"), "{top}");
    assert!(!top.contains("+fifth"), "{top}");
}

#[test]
fn layout_reload_keeps_the_offset_within_a_wrapped_history_entry() {
    let dir = tempdir().unwrap();
    let compact_config = dir.path().join("compact.yml");
    let aligned_config = dir.path().join("aligned.yml");
    fs::write(
        &compact_config,
        "servers: []\ndefaults: {}\nlayout: compact\n",
    )
    .unwrap();
    fs::write(
        &aligned_config,
        "servers: []\ndefaults: {}\nlayout: aligned\nmax_nick_length: 4\n",
    )
    .unwrap();
    let mut compact = TUI::new_test_with_config(24, 5, compact_config.clone());
    let mut aligned = TUI::new_test_with_config(24, 5, aligned_config);
    for tui in [&mut compact, &mut aligned] {
        let chan = ChanNameRef::new("#demo");
        let target = MsgTarget::Chan {
            serv: "server",
            chan,
        };
        let ts = time::at_utc(time::Timespec::new(0, 0));
        tui.new_server_tab("server", None);
        tui.new_chan_tab("server", chan);
        tui.switch("#demo");
        tui.add_privmsg(
            "bob",
            "AAAAABBBBBCCCCCDDDDDEEEEEFFFFFGGGGGHHHHHIIIIIJJJJJKKKKKLLLLLMMMMMNNNNNOOOOO",
            ts,
            &target,
            false,
            false,
        );
        tui.add_client_msg("tail-1", &target);
        tui.add_client_msg("tail-2", &target);
        press(tui, Key::Home);
        press(tui, Key::MouseWheelDown);
        press(tui, Key::MouseWheelDown);
    }
    let reference = screen(&mut aligned, 24, 5);
    reload(
        &mut compact,
        &compact_config,
        "layout: aligned\nmax_nick_length: 4",
    );
    compact.switch("#demo");
    let changed = screen(&mut compact, 24, 5);
    assert_eq!(
        changed.lines().next(),
        reference.lines().next(),
        "changed:\n{changed}\nreference:\n{reference}"
    );
}

#[test]
fn layout_reload_clamps_a_removed_wrapped_offset_to_the_last_row() {
    let dir = tempdir().unwrap();
    let aligned_config = dir.path().join("aligned.yml");
    let compact_config = dir.path().join("compact.yml");
    fs::write(
        &aligned_config,
        "servers: []\ndefaults: {}\nlayout: aligned\nmax_nick_length: 4\n",
    )
    .unwrap();
    fs::write(
        &compact_config,
        "servers: []\ndefaults: {}\nlayout: compact\n",
    )
    .unwrap();
    let mut aligned = TUI::new_test_with_config(24, 5, aligned_config.clone());
    let mut compact = TUI::new_test_with_config(24, 5, compact_config);
    for tui in [&mut aligned, &mut compact] {
        let chan = ChanNameRef::new("#demo");
        let target = MsgTarget::Chan {
            serv: "server",
            chan,
        };
        let ts = time::at_utc(time::Timespec::new(0, 0));
        tui.new_server_tab("server", None);
        tui.new_chan_tab("server", chan);
        tui.switch("#demo");
        tui.add_privmsg(
            "bob",
            "AAAAABBBBBCCCCCDDDDDEEEEEFFFFFGGGGGHHHHHIIIIIJJJJJKKKKKLLLLL",
            ts,
            &target,
            false,
            false,
        );
        tui.add_client_msg("tail-1", &target);
        tui.add_client_msg("tail-2", &target);
        tui.add_client_msg("tail-3", &target);
        tui.add_client_msg("tail-4", &target);
    }
    press(&mut aligned, Key::Home);
    for _ in 0..4 {
        press(&mut aligned, Key::MouseWheelDown);
    }
    press(&mut compact, Key::End);
    for _ in 0..2 {
        press(&mut compact, Key::MouseWheelUp);
    }
    let reference = screen(&mut compact, 24, 5);
    reload(&mut aligned, &aligned_config, "layout: compact");
    aligned.switch("#demo");
    let changed = screen(&mut aligned, 24, 5);
    assert_eq!(
        changed.lines().next(),
        reference.lines().next(),
        "changed:\n{changed}\nreference:\n{reference}"
    );
}
