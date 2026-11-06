use std::cell::RefCell;
use std::rc::Rc;

use crossterm::event::{Event as InputEvent, KeyCode, KeyEvent, KeyModifiers};
use libtiny_common::{Event, MsgSource};
use tokio::sync::mpsc;
use tokio::task::{LocalSet, spawn_local, yield_now};
use tokio_stream::{self as stream, StreamExt};

fn command_input(commands: impl IntoIterator<Item = String>) -> Vec<std::io::Result<InputEvent>> {
    commands
        .into_iter()
        .flat_map(|cmd| {
            [
                Ok(InputEvent::Paste(format!("/{cmd}"))),
                Ok(InputEvent::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))),
            ]
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn commands_and_quit_survive_backpressure_in_order() {
    LocalSet::new()
        .run_until(async {
            let commands = (0..12).map(|i| format!("test-command-{i}"));
            let input = command_input(commands.chain(std::iter::once("quit done".to_owned())));
            let (tui, mut events) = crate::TUI::run_test(80, 5, stream::iter(input));

            // Let the input task fill the ten-event channel before receiving anything.
            yield_now().await;
            assert_eq!(events.len(), 10);
            // The pending send must not retain a mutable borrow of the UI.
            tui.draw();

            for i in 0..12 {
                match events.recv().await {
                    Some(Event::Cmd { cmd, source }) => {
                        assert_eq!(cmd, format!("test-command-{i}"));
                        assert_eq!(
                            source,
                            MsgSource::Serv {
                                serv: "mentions".into()
                            }
                        );
                    }
                    event => panic!("expected command {i}, got {event:?}"),
                }
            }
            assert!(
                matches!(events.recv().await, Some(Event::Quit { msg: Some(msg) }) if msg == "done")
            );
            assert!(events.recv().await.is_none());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn current_tab_tracks_empty_and_restored_tab_lists() {
    LocalSet::new()
        .run_until(async {
            let (tui, _events) = crate::TUI::run_test(20, 3, stream::pending());
            assert_eq!(
                tui.current_tab(),
                Some(MsgSource::Serv {
                    serv: "mentions".into()
                })
            );
            tui.close_server_tab("mentions");
            assert_eq!(tui.current_tab(), None);
            tui.new_server_tab("again", None);
            assert_eq!(
                tui.current_tab(),
                Some(MsgSource::Serv {
                    serv: "again".into()
                })
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn disconnected_event_receiver_ends_input_task_without_panic() {
    LocalSet::new()
        .run_until(async {
            let tui = Rc::new(RefCell::new(crate::tui::TUI::new_test(80, 5)));
            let (sender, receiver) = mpsc::channel(10);
            let input = stream::iter(command_input((0..12).map(|i| format!("test-command-{i}"))))
                .chain(stream::pending());
            let task = spawn_local(crate::input_handler(input, tui.clone(), sender));

            yield_now().await;
            assert_eq!(receiver.len(), 10);
            tui.borrow_mut().draw();
            drop(receiver);
            assert!(
                task.await.is_ok(),
                "input task panicked after receiver closed"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pasted_key_bindings_deliver_every_action_in_order() {
    LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("config.yml");
            std::fs::write(
                &config,
                "servers: []\ndefaults: {}\nkey_map:\n  a:\n    command: first\n  b: input_send\n",
            )
            .unwrap();
            let tui = Rc::new(RefCell::new(crate::tui::TUI::new_test_with_config(
                80, 5, config,
            )));
            let (sender, mut receiver) = mpsc::channel(10);
            let input = stream::iter([
                Ok(InputEvent::Key(KeyEvent::new(
                    KeyCode::Char('z'),
                    KeyModifiers::NONE,
                ))),
                Ok(InputEvent::Paste("aba".into())),
            ]);
            crate::input_handler(input, tui, sender).await;

            assert!(
                matches!(receiver.recv().await, Some(Event::Cmd { cmd, .. }) if cmd == "first")
            );
            assert!(matches!(receiver.recv().await, Some(Event::Msg { msg, .. }) if msg == "z"));
            assert!(
                matches!(receiver.recv().await, Some(Event::Cmd { cmd, .. }) if cmd == "first")
            );
            assert!(receiver.recv().await.is_none());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn single_line_unicode_paste_can_be_sent() {
    LocalSet::new()
        .run_until(async {
            let input = stream::iter([
                Ok(InputEvent::Paste("Aか\u{3099}👩‍💻B".into())),
                Ok(InputEvent::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))),
            ]);
            let (_tui, mut receiver) = crate::TUI::run_test(80, 5, input);
            assert!(matches!(
                receiver.recv().await,
                Some(Event::Msg { msg, .. }) if msg == "Aか\u{3099}👩‍💻B"
            ));
        })
        .await;
}
