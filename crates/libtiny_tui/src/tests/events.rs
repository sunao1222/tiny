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
