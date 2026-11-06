#![allow(clippy::too_many_arguments)]
#![allow(clippy::cognitive_complexity)]

pub mod config;
mod editor;
mod exit_dialogue;
mod input;
mod input_area;
mod key_map;
mod line_split;
mod messaging;
#[doc(hidden)]
pub mod msg_area; // Public to be able to use in an example
mod notifier;
mod tab;
mod terminal;
pub mod test_utils;
mod text;
#[doc(hidden)]
pub mod trie; // Public for benchmarks
pub mod tui; // Public for benchmarks
mod utils;
mod widget;

#[cfg(test)]
mod tests;

use crate::tui::{CmdResult, TUIRet};
use config::TabConfig;
use crossterm::event::Event as InputEvent;
use input::{InputReader, ReaderState};
use libtiny_common::{ChanNameRef, Event, MsgSource, MsgTarget, TabStyle};
pub use notifier::Notifier;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::{Rc, Weak};

use time::Tm;
use tokio::sync::mpsc;
use tokio::task::spawn_local;
use tokio_stream::{Stream, StreamExt};

#[macro_use]
extern crate log;

#[derive(Clone)]
pub struct TUI {
    inner: Weak<RefCell<tui::TUI>>,
}

impl TUI {
    pub fn run(config_path: PathBuf) -> (TUI, mpsc::Receiver<Event>) {
        let tui = Rc::new(RefCell::new(tui::TUI::new(config_path)));
        let inner = Rc::downgrade(&tui);

        let (snd_ev, rcv_ev) = mpsc::channel(10);

        let (reader, receiver) = InputReader::start(None);
        spawn_local(run_input_task(
            InputSource::Live {
                reader: Some(reader),
                receiver,
                queued: VecDeque::new(),
                reader_state: None,
            },
            tui,
            snd_ev,
        ));

        (TUI { inner }, rcv_ev)
    }

    /// Create a test instance backed by Ratatui's in-memory terminal.
    pub fn run_test<S>(width: u16, height: u16, input_stream: S) -> (TUI, mpsc::Receiver<Event>)
    where
        S: Stream<Item = io::Result<InputEvent>> + Unpin + 'static,
    {
        let tui = Rc::new(RefCell::new(tui::TUI::new_test(width, height)));
        let inner = Rc::downgrade(&tui);

        let (snd_ev, rcv_ev) = mpsc::channel(10);

        spawn_local(input_handler(input_stream, tui, snd_ev));

        (TUI { inner }, rcv_ev)
    }

    /// Get the test terminal's front buffer.
    pub fn get_front_buffer(&self) -> ratatui::buffer::Buffer {
        self.inner
            .upgrade()
            .unwrap()
            .borrow_mut()
            .get_front_buffer()
    }
}

enum InputSource {
    Test(Pin<Box<dyn Stream<Item = io::Result<InputEvent>>>>),
    Live {
        reader: Option<InputReader>,
        receiver: mpsc::Receiver<io::Result<InputEvent>>,
        queued: VecDeque<io::Result<InputEvent>>,
        reader_state: Option<ReaderState>,
    },
}

impl InputSource {
    async fn next(&mut self) -> Option<io::Result<InputEvent>> {
        match self {
            Self::Test(stream) => stream.next().await,
            Self::Live {
                receiver, queued, ..
            } => {
                if let Some(event) = queued.pop_front() {
                    Some(event)
                } else {
                    receiver.recv().await
                }
            }
        }
    }

    fn pause(&mut self) -> io::Result<()> {
        if let Self::Live {
            reader,
            receiver,
            queued,
            reader_state,
        } = self
        {
            let stopped = reader.take().expect("input reader is running").stop()?;
            while let Ok(event) = receiver.try_recv() {
                queued.push_back(event);
            }
            queued.extend(stopped.pending);
            *reader_state = Some(stopped.state);
        }
        Ok(())
    }

    fn resume(&mut self) {
        if let Self::Live {
            reader,
            receiver,
            reader_state,
            ..
        } = self
        {
            let (new_reader, new_receiver) = InputReader::start(reader_state.take());
            *reader = Some(new_reader);
            *receiver = new_receiver;
        }
    }
}

async fn input_handler<S>(input_stream: S, tui: Rc<RefCell<tui::TUI>>, snd_ev: mpsc::Sender<Event>)
where
    S: Stream<Item = io::Result<InputEvent>> + Unpin + 'static,
{
    run_input_task(InputSource::Test(Box::pin(input_stream)), tui, snd_ev).await;
}

async fn run_input_task(
    source: InputSource,
    tui: Rc<RefCell<tui::TUI>>,
    snd_ev: mpsc::Sender<Event>,
) {
    if let Err(error) = input_loop(source, tui, snd_ev).await {
        eprintln!("terminal input failed: {error}");
    }
}

async fn dispatch_command(
    tui: &Rc<RefCell<tui::TUI>>,
    sender: &mpsc::Sender<Event>,
    cmd: String,
    source: MsgSource,
) -> bool {
    let result = tui.borrow_mut().try_handle_cmd(&cmd, &source);
    match result {
        CmdResult::Handled => true,
        CmdResult::Pass => sender.send(Event::Cmd { cmd, source }).await.is_ok(),
        CmdResult::Quit(msg) => {
            let _ = sender.send(Event::Quit { msg }).await;
            false
        }
    }
}

async fn edit_input(
    source: &mut InputSource,
    tui: &Rc<RefCell<tui::TUI>>,
    sender: &mpsc::Sender<Event>,
    pasted: String,
) -> io::Result<bool> {
    // No RefCell borrow may survive the thread join or the editor await.
    source.pause()?;
    tui.borrow_mut().suspend()?;
    let (text, cursor) = tui.borrow_mut().take_editor_input();
    let result = match editor::run(text, cursor, &pasted) {
        Ok(receiver) => receiver.await.map_err(io::Error::other)?,
        Err(error) => Err(error),
    };
    let lines = tui.borrow_mut().handle_editor_result(result);
    tui.borrow_mut().resume()?;
    source.resume();
    if let Some((lines, from)) = lines {
        Ok(sender
            .send(Event::Lines {
                lines,
                source: from,
            })
            .await
            .is_ok())
    } else {
        Ok(true)
    }
}

async fn input_loop(
    mut source: InputSource,
    tui: Rc<RefCell<tui::TUI>>,
    sender: mpsc::Sender<Event>,
) -> io::Result<()> {
    while let Some(event) = source.next().await {
        let event = event?;
        let response = tui.borrow_mut().handle_input_event(event);
        let keep_running = match response {
            Some(TUIRet::KeyCommand { cmd, from }) => {
                dispatch_command(&tui, &sender, cmd, from).await
            }
            Some(TUIRet::Input { msg, from }) => {
                if let Some(cmd) = msg.strip_prefix('/') {
                    dispatch_command(&tui, &sender, cmd.to_owned(), from).await
                } else {
                    sender.send(Event::Msg { msg, source: from }).await.is_ok()
                }
            }
            Some(TUIRet::RunEditor { pasted }) => {
                edit_input(&mut source, &tui, &sender, pasted).await?
            }
            None => true,
        };
        if !keep_running {
            break;
        }
        tui.borrow_mut().draw();
    }
    Ok(())
}

macro_rules! delegate {
    ( $(#[$attr:meta])* $name:ident ( $( $x:ident: $t:ty ),* $(,)? ) ) => {
        $(#[$attr])*
        pub fn $name(&self, $($x: $t,)*) {
            if let Some(inner) = self.inner.upgrade() {
                inner.borrow_mut().$name( $( $x, )* );
            }
        }
    }
}

impl TUI {
    delegate!(draw());
    delegate!(
        /// Registers a command name for completion. `name` must not include the leading `/`.
        add_command_completion(name: &str,)
    );
    delegate!(new_server_tab(serv_name: &str, alias: Option<String>));
    delegate!(close_server_tab(serv_name: &str));
    delegate!(new_chan_tab(serv_name: &str, chan: &ChanNameRef));
    delegate!(close_chan_tab(serv_name: &str, chan: &ChanNameRef));
    delegate!(close_user_tab(serv_name: &str, nick: &str));
    delegate!(add_client_msg(msg: &str, target: &MsgTarget));
    delegate!(add_msg(msg: &str, ts: Tm, target: &MsgTarget));
    delegate!(add_err_msg(msg: &str, ts: Tm, target: &MsgTarget));
    delegate!(add_client_err_msg(msg: &str, target: &MsgTarget));
    delegate!(clear_nicks(serv_name: &str));
    delegate!(set_nick(serv_name: &str, new_nick: &str));
    delegate!(
        add_privmsg(
            sender: &str,
            msg: &str,
            ts: Tm,
            target: &MsgTarget,
            highlight: bool,
            is_action: bool,
        )
    );
    delegate!(add_nick(nick: &str, ts: Option<Tm>, target: &MsgTarget));
    delegate!(remove_nick(nick: &str, ts: Option<Tm>, target: &MsgTarget));
    delegate!(rename_nick(old_nick: &str, new_nick: &str, ts: Tm, target: &MsgTarget));
    delegate!(set_topic(topic: &str, ts: Tm, serv_name: &str, chan_name: &ChanNameRef));
    delegate!(set_tab_style(style: TabStyle, target: &MsgTarget));

    pub fn get_tab_config(&self, serv_name: &str, chan_name: Option<&ChanNameRef>) -> TabConfig {
        self.inner
            .upgrade()
            .map(|tui| tui.borrow().get_tab_config(serv_name, chan_name))
            .unwrap_or_default()
    }

    pub fn set_tab_config(
        &self,
        serv_name: &str,
        chan_name: Option<&ChanNameRef>,
        config: TabConfig,
    ) {
        if let Some(tui) = self.inner.upgrade() {
            tui.borrow_mut()
                .set_tab_config(serv_name, chan_name, config);
        }
    }

    pub fn user_tab_exists(&self, serv_name: &str, nick: &str) -> bool {
        match self.inner.upgrade() {
            Some(tui) => tui.borrow().user_tab_exists(serv_name, nick),
            None => false,
        }
    }

    pub fn current_tab(&self) -> Option<MsgSource> {
        self.inner
            .upgrade()
            .map(|tui| tui.borrow().current_tab().clone())
    }
}
