#![allow(
    clippy::cognitive_complexity,
    clippy::new_without_default,
    clippy::too_many_arguments
)]
// https://github.com/rust-lang/rust-clippy/issues/7526
#![allow(clippy::needless_collect)]

use std::borrow::Borrow;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::str::{self, SplitWhitespace};
use time::Tm;

use crate::config::{Colors, Config, Style, TabConfig, TabConfigs, parse_config};
use crate::editor;
use crate::key_map::{KeyAction, KeyMap};
use crate::messaging::{MessagingUI, Timestamp};
use crate::msg_area::Layout;
use crate::notifier::Notifier;
use crate::tab::{Tab, tab_style};
use crate::terminal::TerminalSession;
use crate::trie::Trie;
use crate::widget::WidgetRet;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use libtiny_common::{ChanNameRef, MsgSource, MsgTarget, TabStyle};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout as RatatuiLayout, Rect};
use ratatui::widgets::Widget;

#[derive(Debug)]
pub(crate) enum TUIRet {
    /// A command was submitted, either directly, or via a key bound to a command.
    ///
    /// `cmd` won't have an initial '/'.
    KeyCommand { cmd: String, from: MsgSource },

    /// A message was sent. `msg` will have at least one character.
    Input { msg: String, from: MsgSource },

    /// The input task must suspend terminal input before launching the editor.
    RunEditor { pasted: String },
}

const LEFT_ARROW: char = '<';
const RIGHT_ARROW: char = '>';

struct CmdUsage {
    name: &'static str,
    description: &'static str,
    usage: &'static str,
}

impl CmdUsage {
    const fn new(name: &'static str, description: &'static str, usage: &'static str) -> CmdUsage {
        CmdUsage {
            name,
            description,
            usage,
        }
    }
}

const QUIT_CMD: CmdUsage = CmdUsage::new("quit", "Quit tiny", "`/quit` or `/quit <reason>`");
const CLEAR_CMD: CmdUsage = CmdUsage::new("clear", "Clears current tab", "`/clear`");
const IGNORE_CMD: CmdUsage = CmdUsage::new("ignore", "Ignore join/quit messages", "`/ignore`");
const NOTIFY_CMD: CmdUsage = CmdUsage::new(
    "notify",
    "Set channel notifications",
    "`/notify [off|mentions|messages]`",
);
const SWITCH_CMD: CmdUsage = CmdUsage::new("switch", "Switches to tab", "`/switch <tab name>`");
const RELOAD_CMD: CmdUsage = CmdUsage::new("reload", "Reloads config file", "`/reload`");

const TUI_COMMANDS: [CmdUsage; 6] = [
    QUIT_CMD, CLEAR_CMD, IGNORE_CMD, NOTIFY_CMD, SWITCH_CMD, RELOAD_CMD,
];

/// Public test and benchmark handle, with terminal ownership separate from UI state.
pub struct TUI {
    state: TuiState,
    terminal: TerminalSession,
}

/// UI state shared by the production and test rendering backends.
pub struct TuiState {
    /// Color scheme
    colors: Colors,
    nerd_font: bool,

    /// Max number of message lines
    scrollback: usize,

    /// Messaging area layout: aligned or compact
    msg_layout: Layout,

    tabs: Vec<Tab>,
    active_idx: usize,
    width: i32,
    height: i32,

    key_map: KeyMap,
    command_completions: Trie,

    /// Config file path
    config_path: Option<PathBuf>,

    /// TabConfig settings loaded from config file
    tab_configs: TabConfigs,
}

pub(crate) enum CmdResult {
    /// Command executed successfully by the TUI.
    Handled,

    /// Pass command through to the caller.
    Pass,

    /// Quit command was executed, with the payload as the quit message.
    Quit(Option<String>),
}

impl Deref for TUI {
    type Target = TuiState;

    fn deref(&self) -> &TuiState {
        &self.state
    }
}

impl DerefMut for TUI {
    fn deref_mut(&mut self) -> &mut TuiState {
        &mut self.state
    }
}

impl TUI {
    pub(crate) fn new(config_path: PathBuf) -> TUI {
        let terminal = TerminalSession::new().expect("failed to initialize terminal");
        Self::with_terminal(Some(config_path), terminal)
    }

    /// Create a test instance backed by Ratatui's in-memory terminal.
    pub fn new_test(w: u16, h: u16) -> TUI {
        let terminal = TerminalSession::new_test(w, h).expect("failed to initialize test terminal");
        Self::with_terminal(None, terminal)
    }

    #[cfg(test)]
    pub(crate) fn new_test_with_config(w: u16, h: u16, config_path: PathBuf) -> TUI {
        let terminal = TerminalSession::new_test(w, h).expect("failed to initialize test terminal");
        Self::with_terminal(Some(config_path), terminal)
    }

    fn with_terminal(config_path: Option<PathBuf>, terminal: TerminalSession) -> TUI {
        let (width, height) = terminal.size().expect("failed to get terminal size");
        TUI {
            state: TuiState::new(config_path, width, height),
            terminal,
        }
    }

    pub(crate) fn get_front_buffer(&mut self) -> Buffer {
        let state = &mut self.state;
        self.terminal
            .front_buffer(|frame| render_frame(state, frame))
            .expect("failed to read terminal buffer")
    }

    pub(crate) fn suspend(&mut self) -> std::io::Result<()> {
        self.terminal.suspend()
    }

    pub(crate) fn resume(&mut self) -> std::io::Result<()> {
        let (width, height) = self.terminal.resume()?;
        self.state.resize(width, height);
        self.try_draw()
    }

    pub(crate) fn handle_input_event(&mut self, event: Event) -> Option<TUIRet> {
        if let Event::Resize(width, height) = event {
            self.set_size(width, height);
            None
        } else {
            self.state.handle_input_event(event)
        }
    }

    /// Set the terminal size; in tests this resizes the in-memory backend.
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.terminal
            .set_size(width, height)
            .expect("failed to resize terminal");
        self.state.resize(width, height);
        self.draw();
    }

    fn try_draw(&mut self) -> std::io::Result<()> {
        let state = &mut self.state;
        self.terminal.draw(|frame| render_frame(state, frame))
    }

    pub fn draw(&mut self) {
        self.try_draw().expect("failed to draw terminal");
    }
}

fn render_frame(state: &mut TuiState, frame: &mut ratatui::Frame) {
    let mut cursor = None;
    let area = frame.area();
    frame.render_widget(
        Screen {
            state,
            cursor: &mut cursor,
        },
        area,
    );
    if let Some((x, y)) = cursor {
        frame.set_cursor_position((x, y));
    }
}

impl TuiState {
    #[cfg(test)]
    pub(crate) fn set_layout(&mut self, layout: Layout) {
        self.msg_layout = layout
    }

    pub(crate) fn current_tab(&self) -> Option<&MsgSource> {
        self.tabs.get(self.active_idx).map(|tab| &tab.src)
    }

    #[cfg(test)]
    pub(crate) fn get_tabs(&self) -> &[Tab] {
        &self.tabs
    }

    fn new(config_path: Option<PathBuf>, width: u16, height: u16) -> TuiState {
        let mut command_completions = Trie::new();
        for command in &TUI_COMMANDS {
            command_completions.insert(command.name);
        }

        let mut tui = TuiState {
            colors: Colors::default(),
            nerd_font: false,
            scrollback: usize::MAX,
            msg_layout: Layout::Compact,
            tabs: Vec::new(),
            active_idx: 0,
            width: i32::from(width),
            height: i32::from(height),
            key_map: KeyMap::default(),
            command_completions,
            config_path,
            tab_configs: TabConfigs::default(),
        };

        // Init "mentions" tab. This needs to happen right after creating the TUI to be able to
        // show any errors in TUI.
        tui.new_server_tab("mentions", None);
        tui.add_client_msg(
            "Any mentions to you will be listed here.",
            &MsgTarget::Server { serv: "mentions" },
        );

        tui.reload_config();
        tui
    }

    pub(crate) fn add_command_completion(&mut self, name: &str) {
        self.command_completions.insert(name);
    }

    fn ignore(&mut self, src: &MsgSource) {
        let (target, ignore) = match src {
            MsgSource::Serv { serv } => {
                let mut config = self.get_tab_config(serv, None);
                let new_ignore = config.toggle_ignore();
                self.tab_configs.set_by_server(serv, config);
                (MsgTarget::AllServTabs { serv }, new_ignore)
            }
            MsgSource::Chan { serv, chan } => {
                let mut config = self.get_tab_config(serv, Some(chan));
                let new_ignore = config.toggle_ignore();
                self.tab_configs.set(serv, Some(chan), config);
                (MsgTarget::Chan { serv, chan }, new_ignore)
            }
            MsgSource::User { serv, nick } => {
                let nick_chan = ChanNameRef::new(nick);
                let mut config = self.get_tab_config(serv, Some(nick_chan));
                let new_ignore = config.toggle_ignore();
                self.tab_configs.set(serv, Some(nick_chan), config);
                (MsgTarget::User { serv, nick }, new_ignore)
            }
        };
        let msg = if ignore {
            "Ignore enabled"
        } else {
            "Ignore disabled"
        };
        self.add_client_notify_msg(msg, &target);
    }

    fn notify(&mut self, words: &mut SplitWhitespace, src: &MsgSource) {
        if !cfg!(feature = "desktop-notifications") {
            self.add_client_msg(
                "Desktop notification support is disabled in this build. \
                Please see https://github.com/osa1/tiny/#installation for \
                instructions on enabling desktop notifications or get a \
                pre-built binary with libdbus in https://github.com/osa1/tiny/releases.",
                &MsgTarget::CurrentTab,
            );
            return;
        }

        let words: Vec<&str> = words.collect();

        let mut show_usage = || {
            self.add_client_err_msg(
                &format!("Usage: {}", NOTIFY_CMD.usage),
                &MsgTarget::CurrentTab,
            )
        };

        if words.is_empty() {
            self.show_notify_mode(src);
        } else if words.len() != 1 {
            show_usage();
        } else {
            let notifier = match words[0] {
                "off" => {
                    self.add_client_notify_msg("Notifications turned off", &MsgTarget::CurrentTab);
                    Notifier::Off
                }
                "mentions" => {
                    self.add_client_notify_msg(
                        "Notifications enabled for mentions",
                        &MsgTarget::CurrentTab,
                    );
                    Notifier::Mentions
                }
                "messages" => {
                    self.add_client_notify_msg(
                        "Notifications enabled for all messages",
                        &MsgTarget::CurrentTab,
                    );
                    Notifier::Messages
                }
                _ => {
                    return show_usage();
                }
            };
            // can't use `MsgSource::to_target` here, `Serv` case is different
            let tab_target = match src {
                MsgSource::Serv { serv } => MsgTarget::AllServTabs { serv },
                MsgSource::Chan { serv, chan } => MsgTarget::Chan {
                    serv,
                    chan: chan.borrow(),
                },
                MsgSource::User { serv, nick } => MsgTarget::User { serv, nick },
            };
            self.set_notifier(notifier, &tab_target);
        }
    }

    pub(crate) fn try_handle_cmd(&mut self, cmd: &str, src: &MsgSource) -> CmdResult {
        let mut words = cmd.split_whitespace();
        match words.next() {
            Some("clear") => {
                self.clear(&src.to_target());
                CmdResult::Handled
            }
            Some("ignore") => {
                self.ignore(src);
                CmdResult::Handled
            }
            Some("notify") => {
                self.notify(&mut words, src);
                CmdResult::Handled
            }
            Some("switch") => {
                match words.next() {
                    Some(s) => self.switch(s),
                    None => self.add_client_err_msg(
                        &format!("Usage: {}", SWITCH_CMD.usage),
                        &MsgTarget::CurrentTab,
                    ),
                }
                CmdResult::Handled
            }
            Some("reload") => {
                self.reload_config();
                self.add_client_notify_msg("Reloaded config file.", &MsgTarget::CurrentTab);
                CmdResult::Handled
            }
            Some("help") => {
                self.add_client_msg("TUI Commands: ", &MsgTarget::CurrentTab);
                for cmd in TUI_COMMANDS.iter() {
                    self.add_client_msg(
                        &format!(
                            "/{:<10} - {:<25} - Usage: {}",
                            cmd.name, cmd.description, cmd.usage
                        ),
                        &MsgTarget::CurrentTab,
                    );
                }
                // Fall through to print help for cmd.rs commands
                CmdResult::Pass
            }
            Some("quit") => {
                // Note: `SplitWhitespace::as_str` could be used here instead, when it gets stabilized.
                let reason: String = cmd.chars().skip("quit ".len()).collect();

                if reason.is_empty() {
                    CmdResult::Quit(None)
                } else {
                    CmdResult::Quit(Some(reason))
                }
            }
            _ => CmdResult::Pass,
        }
    }

    pub(crate) fn load_config(&mut self) -> Option<Config> {
        if let Some(ref config_path) = self.config_path {
            match parse_config(config_path) {
                Err(err) => {
                    self.add_client_err_msg(
                        &format!("Can't parse TUI config: {err}"),
                        &MsgTarget::CurrentTab,
                    );
                    None
                }
                Ok(config) => Some(config),
            }
        } else {
            None
        }
    }

    fn apply_config(&mut self, config: Option<Config>) {
        if let Some(config) = config {
            self.tab_configs = TabConfigs::from(&config);
            let Config {
                colors,
                nerd_font,
                scrollback,
                max_nick_length,
                key_map,
                layout,
                ..
            } = config;
            self.set_colors(colors);
            self.nerd_font = nerd_font;
            self.scrollback = scrollback.max(1);
            self.key_map.load(&key_map.unwrap_or_default());
            if let Some(layout) = layout {
                match layout {
                    crate::config::Layout::Compact => self.msg_layout = Layout::Compact,
                    crate::config::Layout::Aligned => {
                        self.msg_layout = Layout::Aligned {
                            max_nick_len: max_nick_length,
                        }
                    }
                }
            }
            for tab in &mut self.tabs {
                tab.widget.apply_config(self.scrollback, self.msg_layout);
            }
        }
    }

    fn reload_config(&mut self) {
        let config = self.load_config();
        self.apply_config(config);
    }

    fn set_colors(&mut self, colors: Colors) {
        self.colors = colors;
    }

    fn new_tab(&mut self, idx: usize, src: MsgSource, alias: Option<String>) {
        let visible_name = alias.unwrap_or_else(|| match &src {
            MsgSource::Serv { serv } => serv.to_owned(),
            MsgSource::Chan { chan, .. } => chan.display().to_owned(),
            MsgSource::User { nick, .. } => nick.to_owned(),
        });

        let switch = {
            // Maps a switch key to number of times it's used
            let mut switch_keys: HashMap<char, u16> = HashMap::with_capacity(self.tabs.len());

            for tab in &self.tabs {
                if let Some(key) = tab.switch {
                    *switch_keys.entry(key).or_default() += 1;
                }
            }

            // From the characters in tab name, find the one that is used the least
            let mut new_tab_switch_char: Option<(char, u16)> = None;
            for ch in visible_name.chars() {
                if !ch.is_alphabetic() {
                    continue;
                }
                match switch_keys.get(&ch).copied() {
                    None => {
                        new_tab_switch_char = Some((ch, 0));
                        break;
                    }
                    Some(n_uses) => match new_tab_switch_char {
                        None => {
                            new_tab_switch_char = Some((ch, n_uses));
                        }
                        Some((_, new_tab_switch_char_n_uses)) => {
                            if new_tab_switch_char_n_uses > n_uses {
                                new_tab_switch_char = Some((ch, n_uses));
                            }
                        }
                    },
                }
            }
            new_tab_switch_char.map(|(ch, _)| ch)
        };

        if !self.tabs.is_empty() && idx <= self.active_idx {
            self.active_idx += 1;
        }
        self.tabs.insert(
            idx,
            Tab {
                visible_name,
                widget: MessagingUI::new(
                    self.width,
                    messaging_height(self.height),
                    self.scrollback,
                    self.msg_layout,
                ),
                src,
                style: TabStyle::Normal,
                switch,
            },
        );
    }

    /// Returns index of the new tab if a new tab is created.
    pub fn new_server_tab(&mut self, serv: &str, alias: Option<String>) -> Option<usize> {
        match self.find_serv_tab_idx(serv) {
            None => {
                let tab_idx = self.tabs.len();
                self.new_tab(
                    tab_idx,
                    MsgSource::Serv {
                        serv: serv.to_owned(),
                    },
                    alias,
                );
                Some(tab_idx)
            }
            Some(_) => None,
        }
    }

    /// Closes a server tab and all associated channel tabs.
    pub(crate) fn close_server_tab(&mut self, serv: &str) {
        if let Some(start) = self.find_serv_tab_idx(serv) {
            let (_, end) = self.server_tab_range(start);
            self.remove_tabs(start, end);
        }
    }

    /// Returns index of the new tab if a new tab is created.
    pub(crate) fn new_chan_tab(&mut self, serv: &str, chan: &ChanNameRef) -> Option<usize> {
        match self.find_chan_tab_idx(serv, chan) {
            None => match self.find_last_serv_tab_idx(serv) {
                None => {
                    self.new_server_tab(serv, None);
                    self.new_chan_tab(serv, chan)
                }
                Some(serv_tab_idx) => {
                    let tab_idx = serv_tab_idx + 1;
                    self.new_tab(
                        tab_idx,
                        MsgSource::Chan {
                            serv: serv.to_owned(),
                            chan: chan.to_owned(),
                        },
                        None,
                    );
                    // Insertion preserves the selected tab in new_tab.
                    if let Some(nick) = self.tabs[serv_tab_idx].widget.get_nick() {
                        self.tabs[tab_idx].widget.set_nick(nick);
                    }
                    Some(tab_idx)
                }
            },
            Some(_) => None,
        }
    }

    pub(crate) fn close_chan_tab(&mut self, serv: &str, chan: &ChanNameRef) {
        if let Some(idx) = self.find_chan_tab_idx(serv, chan) {
            self.remove_tabs(idx, idx + 1);
        }
    }

    /// Returns index of the new tab if a new tab is created.
    pub(crate) fn new_user_tab(&mut self, serv: &str, nick: &str) -> Option<usize> {
        match self.find_user_tab_idx(serv, nick) {
            None => match self.find_last_serv_tab_idx(serv) {
                None => {
                    self.new_server_tab(serv, None);
                    self.new_user_tab(serv, nick)
                }
                Some(tab_idx) => {
                    self.new_tab(
                        tab_idx + 1,
                        MsgSource::User {
                            serv: serv.to_owned(),
                            nick: nick.to_owned(),
                        },
                        None,
                    );
                    if let Some(nick) = self.tabs[tab_idx].widget.get_nick() {
                        self.tabs[tab_idx + 1].widget.set_nick(nick);
                    }
                    self.tabs[tab_idx + 1].widget.join(nick, None, false);
                    Some(tab_idx + 1)
                }
            },
            Some(_) => None,
        }
    }

    pub(crate) fn close_user_tab(&mut self, serv: &str, nick: &str) {
        if let Some(idx) = self.find_user_tab_idx(serv, nick) {
            self.remove_tabs(idx, idx + 1);
        }
    }

    fn remove_tabs(&mut self, start: usize, end: usize) {
        let selected_removed = (start..end).contains(&self.active_idx);
        self.tabs.drain(start..end);
        if self.tabs.is_empty() {
            self.active_idx = 0;
        } else if selected_removed {
            self.active_idx = start.saturating_sub(1).min(self.tabs.len() - 1);
            self.tabs[self.active_idx].set_style(TabStyle::Normal);
        } else if self.active_idx >= end {
            self.active_idx -= end - start;
        }
    }

    pub(crate) fn handle_input_event(&mut self, event: Event) -> Option<TUIRet> {
        if self.tabs.is_empty() {
            return None;
        }
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.keypressed(key),
            Event::Mouse(mouse) => {
                let action = self.key_map.get_mouse(mouse.kind)?;
                self.dispatch_action(action)
            }
            Event::Paste(text) => Some(TUIRet::RunEditor { pasted: text }),
            _ => None,
        }
    }

    pub(crate) fn handle_editor_result(
        &mut self,
        editor_ret: editor::Result<Vec<String>>,
    ) -> Option<(Vec<String>, MsgSource)> {
        if self.tabs.is_empty() {
            return None;
        }
        match editor_ret {
            Err(err) => {
                self.handle_editor_err(err);
                None
            }
            Ok(lines) => {
                let tab = &mut self.tabs[self.active_idx].widget;
                // If there's only one line just add it to the input field, do not send it
                if lines.len() == 1 {
                    tab.set_input_field(&lines[0]);
                    None
                } else {
                    // Otherwise add the lines to text field history and send it
                    for line in &lines {
                        tab.add_input_field_history(line);
                    }
                    Some((lines, self.tabs[self.active_idx].src.clone()))
                }
            }
        }
    }

    pub(crate) fn take_editor_input(&mut self) -> (String, usize) {
        if self.tabs.is_empty() {
            return (String::new(), 0);
        }
        self.tabs[self.active_idx].widget.flush_input_field()
    }

    fn handle_editor_err(&mut self, err: editor::Error) {
        use std::env::VarError;

        let editor::Error {
            text_field_contents,
            cursor,
            kind,
        } = err;

        match kind {
            editor::ErrorKind::Io(err) => {
                self.add_client_err_msg(
                    &format!("Error while running $EDITOR: {err:?}"),
                    &MsgTarget::CurrentTab,
                );
            }
            editor::ErrorKind::Var(VarError::NotPresent) => {
                self.add_client_err_msg(
                    "Can't paste multi-line string: \
                             make sure your $EDITOR is set",
                    &MsgTarget::CurrentTab,
                );
            }
            editor::ErrorKind::Var(VarError::NotUnicode(_)) => {
                self.add_client_err_msg(
                    "Can't paste multi-line string: \
                             can't parse $EDITOR (not unicode)",
                    &MsgTarget::CurrentTab,
                );
            }
        }

        // Restore text field contents
        let tab = &mut self.tabs[self.active_idx].widget;
        tab.set_input_field(&text_field_contents);
        tab.set_cursor(cursor);
    }

    fn keypressed(&mut self, key: KeyEvent) -> Option<TUIRet> {
        let key_action = self.key_map.get(&key).or_else(|| match key.code {
            KeyCode::Char(c) if key.modifiers == KeyModifiers::ALT => Some(KeyAction::TabGoto(c)),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(KeyAction::Input(c))
            }
            _ => None,
        })?;
        self.dispatch_action(key_action)
    }

    fn dispatch_action(&mut self, key_action: KeyAction) -> Option<TUIRet> {
        match self.tabs[self.active_idx]
            .widget
            .keypressed(&key_action, &self.command_completions)
        {
            WidgetRet::KeyHandled => None,

            WidgetRet::KeyIgnored => self.handle_keypress(key_action),

            WidgetRet::Command(cmd) => Some(TUIRet::KeyCommand {
                cmd,
                from: self.tabs[self.active_idx].src.clone(),
            }),

            WidgetRet::Input(input) => Some(TUIRet::Input {
                msg: input,
                from: self.tabs[self.active_idx].src.clone(),
            }),

            WidgetRet::Remove => unimplemented!(),
        }
    }

    fn handle_keypress(&mut self, key_action: KeyAction) -> Option<TUIRet> {
        match key_action {
            KeyAction::RunEditor => {
                return Some(TUIRet::RunEditor {
                    pasted: String::new(),
                });
            }

            KeyAction::TabNext => {
                self.next_tab();
            }

            KeyAction::TabPrev => {
                self.prev_tab();
            }

            KeyAction::TabMoveLeft => {
                self.move_tab_left();
            }

            KeyAction::TabMoveRight => {
                self.move_tab_right();
            }

            KeyAction::TabGoto(c) => self.go_to_tab(c),

            _ => {}
        }
        None
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.width = i32::from(width);
        self.height = i32::from(height);
        self.resize_();
    }

    fn resize_(&mut self) {
        for tab in &mut self.tabs {
            tab.widget.resize(self.width, messaging_height(self.height));
        }
    }

    pub(crate) fn get_tab_config(
        &self,
        serv_name: &str,
        chan_name: Option<&ChanNameRef>,
    ) -> TabConfig {
        let configs = &self.tab_configs;
        configs
            .get(serv_name, chan_name) // tries to get
            .or_else(|| configs.get(serv_name, None))
            .or_else(|| configs.get("_defaults", None))
            .unwrap_or_default()
    }

    pub(crate) fn set_tab_config(
        &mut self,
        serv_name: &str,
        chan_name: Option<&ChanNameRef>,
        config: TabConfig,
    ) {
        self.tab_configs.set(serv_name, chan_name, config)
    }
}

////////////////////////////////////////////////////////////////////////////////
// Rendering

fn tab_bar_height(height: i32) -> i32 {
    if height >= 4 {
        3
    } else if height == 3 {
        2
    } else {
        1
    }
}

fn messaging_height(height: i32) -> i32 {
    (height - tab_bar_height(height)).max(0)
}

fn marker(style: TabStyle) -> Option<char> {
    match style {
        TabStyle::Normal => None,
        TabStyle::JoinOrPart => Some('+'),
        TabStyle::NewMsg => Some('*'),
        TabStyle::Highlight => Some('!'),
    }
}

struct Screen<'a> {
    state: &'a mut TuiState,
    cursor: &'a mut Option<(u16, u16)>,
}

impl Widget for Screen<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        *self.cursor = self.state.render(area, buffer);
    }
}

impl TuiState {
    fn row_next(&self, idx: usize, end: usize, servers: bool) -> usize {
        if servers {
            self.server_tab_range(idx).1.min(end)
        } else {
            idx + 1
        }
    }

    fn row_prev(&self, idx: usize, servers: bool) -> usize {
        if servers {
            self.server_tab_range(idx - 1).0
        } else {
            idx - 1
        }
    }

    fn row_style(&self, idx: usize, servers: bool) -> TabStyle {
        if servers {
            let (_, end) = self.server_tab_range(idx);
            self.tabs[idx..end]
                .iter()
                .map(|tab| tab.style)
                .max()
                .unwrap_or(TabStyle::Normal)
        } else {
            self.tabs[idx].style
        }
    }

    fn item_width(&self, idx: usize, servers: bool) -> i32 {
        let icon = self.icon(idx);
        let icon_width = crate::text::text_width(icon) as i32;
        2 + icon_width
            + 1
            + self.tabs[idx].width()
            + i32::from(servers && marker(self.row_style(idx, true)).is_some())
    }

    fn icon(&self, idx: usize) -> &'static str {
        match (&self.tabs[idx].src, self.nerd_font) {
            (MsgSource::Serv { .. }, false) => "●",
            (MsgSource::Chan { .. }, false) => "·",
            (MsgSource::User { .. }, false) => "@",
            (MsgSource::Serv { .. }, true) => "",
            (MsgSource::Chan { .. }, true) => "",
            (MsgSource::User { .. }, true) => "",
        }
    }

    fn draw_row(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        start: usize,
        end: usize,
        focus: usize,
        servers: bool,
        max_focus_offset: Option<i32>,
        tree_area: Option<Rect>,
    ) -> i32 {
        let width = i32::from(area.width);
        let origin = i32::from(area.x);
        let y = i32::from(area.y);
        if width == 0 || start == end {
            return origin;
        }
        if width <= 2 {
            if let Some(tree) = tree_area {
                let label = if focus == self.active_idx {
                    self.colors.tab_active
                } else {
                    tab_style(self.tabs[focus].style, &self.colors)
                };
                crate::text::draw_styled_chars_clipped(
                    buffer,
                    tree,
                    origin,
                    i32::from(tree.y),
                    std::iter::once(('│', label)),
                );
            }
            self.tabs[focus].draw(
                buffer,
                &self.colors,
                area,
                origin,
                y,
                focus == self.active_idx,
            );
            return origin;
        }

        let mut left = focus;
        let mut right = self.row_next(focus, end, servers);
        let mut used = self.item_width(focus, servers);
        let fits = |used: i32, left: usize, right: usize| {
            used + i32::from(left > start) + i32::from(right < end) <= width
        };
        if fits(used, left, right) {
            let mut before_focus_width = 0;
            while left > start {
                let candidate = self.row_prev(left, servers);
                let added = self.item_width(candidate, servers) + 1;
                if !fits(used + added, candidate, right)
                    || max_focus_offset.is_some_and(|limit| {
                        before_focus_width + added + i32::from(candidate > start) > limit
                    })
                {
                    break;
                }
                left = candidate;
                used += added;
                before_focus_width += added;
            }
            while right < end {
                let next = self.row_next(right, end, servers);
                let added = self.item_width(right, servers) + 1;
                if !fits(used + added, left, next) {
                    break;
                }
                right = next;
                used += added;
            }
        }
        let has_left = left > start;
        let has_right = right < end;
        let mut x = origin + i32::from(has_left);
        let content_right = origin + width - i32::from(has_right);
        let clip = Rect::new(x as u16, area.y, (content_right - x).max(0) as u16, 1);
        if has_left {
            let mut max = TabStyle::Normal;
            let mut i = start;
            while i < left {
                max = max.max(self.row_style(i, servers));
                i = self.row_next(i, end, servers);
            }
            crate::text::draw_styled_chars_clipped(
                buffer,
                area,
                origin,
                y,
                std::iter::once((LEFT_ARROW, tab_style(max, &self.colors))),
            );
        }
        if has_right {
            let mut max = TabStyle::Normal;
            let mut i = right;
            while i < end {
                max = max.max(self.row_style(i, servers));
                i = self.row_next(i, end, servers);
            }
            crate::text::draw_styled_chars_clipped(
                buffer,
                area,
                origin + width - 1,
                y,
                std::iter::once((RIGHT_ARROW, tab_style(max, &self.colors))),
            );
        }
        let mut previous_tree_x = origin;
        if let Some(tree) = tree_area.filter(|_| has_left) {
            crate::text::draw_styled_chars_clipped(
                buffer,
                tree,
                origin,
                i32::from(tree.y),
                std::iter::once(('└', self.colors.tab_normal)),
            );
        }
        let mut focus_x = x;
        let mut i = left;
        while i < right {
            if i == focus {
                focus_x = x;
            }
            if let Some(tree) = tree_area {
                let tree_y = i32::from(tree.y);
                crate::text::draw_styled_chars_clipped(
                    buffer,
                    tree,
                    previous_tree_x + 1,
                    tree_y,
                    (previous_tree_x + 1..x).map(|_| ('─', self.colors.tab_normal)),
                );
                let last = self.row_next(i, end, false) >= right;
                let branch = if x == origin {
                    if last { '│' } else { '├' }
                } else if last {
                    '┐'
                } else {
                    '┬'
                };
                let label = if i == self.active_idx {
                    self.colors.tab_active
                } else {
                    tab_style(self.tabs[i].style, &self.colors)
                };
                crate::text::draw_styled_chars_clipped(
                    buffer,
                    tree,
                    x,
                    tree_y,
                    std::iter::once((branch, label)),
                );
                previous_tree_x = x;
            }
            let item_w = self.item_width(i, servers);
            if i == focus && item_w > content_right - x {
                let mark = if servers {
                    marker(self.row_style(i, true))
                } else {
                    None
                };
                let reserve = i32::from(mark.is_some() && content_right - x >= 2);
                let name_clip = Rect::new(
                    x as u16,
                    area.y,
                    (content_right - x - reserve).max(0) as u16,
                    1,
                );
                self.tabs[i].draw(buffer, &self.colors, name_clip, x, y, i == self.active_idx);
                if reserve != 0 {
                    crate::text::draw_styled_chars_clipped(
                        buffer,
                        clip,
                        content_right - 1,
                        y,
                        std::iter::once((
                            mark.unwrap(),
                            tab_style(self.row_style(i, true), &self.colors),
                        )),
                    );
                }
            } else {
                self.draw_item(buffer, clip, x, y, i, servers);
            }
            x += item_w + 1;
            i = self.row_next(i, end, servers);
        }
        focus_x
    }

    fn draw_item(
        &self,
        buffer: &mut Buffer,
        clip: Rect,
        mut x: i32,
        y: i32,
        idx: usize,
        servers: bool,
    ) {
        let tab = &self.tabs[idx];
        let active_group = tab.src.serv_name() == self.tabs[self.active_idx].src.serv_name();
        let badge = if active_group {
            self.colors.tab_group_active
        } else {
            self.colors.tab_group
        };
        let label = if idx == self.active_idx {
            self.colors.tab_active
        } else {
            tab_style(tab.style, &self.colors)
        };
        let icon = self.icon(idx);
        crate::text::draw_styled_chars_clipped(
            buffer,
            clip,
            x,
            y,
            std::iter::once(' ')
                .chain(icon.chars())
                .chain(std::iter::once(' '))
                .map(|c| (c, badge)),
        );
        x += 2 + crate::text::text_width(icon) as i32;
        let separator = if self.nerd_font && badge.bg != ratatui::style::Color::Reset {
            ''
        } else {
            ' '
        };
        let separator_style = Style {
            fg: badge.bg,
            bg: label.bg,
            modifiers: ratatui::style::Modifier::empty(),
        };
        crate::text::draw_styled_chars_clipped(
            buffer,
            clip,
            x,
            y,
            std::iter::once((separator, separator_style)),
        );
        x += 1;
        tab.draw(buffer, &self.colors, clip, x, y, idx == self.active_idx);
        x += tab.width();
        if servers && let Some(ch) = marker(self.row_style(idx, true)) {
            crate::text::draw_styled_chars_clipped(
                buffer,
                clip,
                x,
                y,
                std::iter::once((ch, tab_style(self.row_style(idx, true), &self.colors))),
            );
        }
    }

    fn render(&mut self, area: Rect, buffer: &mut Buffer) -> Option<(u16, u16)> {
        buffer.set_style(area, self.colors.clear);
        if area.width == 0 || area.height < 2 || self.tabs.is_empty() {
            return None;
        }
        let bar_height = tab_bar_height(i32::from(area.height)) as u16;
        let layout = RatatuiLayout::vertical([Constraint::Min(0), Constraint::Length(bar_height)])
            .split(area);
        let messages = layout[0];
        let bar = layout[1];
        let cursor = self.tabs[self.active_idx].widget.draw(
            buffer,
            &self.colors,
            i32::from(messages.x),
            i32::from(messages.y),
        );
        if bar_height == 1 {
            self.tabs[self.active_idx].draw(
                buffer,
                &self.colors,
                bar,
                i32::from(bar.x),
                i32::from(bar.y),
                true,
            );
        } else {
            let (parent, end) = self.server_tab_range(self.active_idx);
            let has_children = parent + 1 < end;
            let top = Rect::new(bar.x, bar.y, bar.width, 1);
            let focus_x = self.draw_row(
                buffer,
                top,
                0,
                self.tabs.len(),
                parent,
                true,
                has_children.then_some(i32::from(bar.width) / 2),
                None,
            );
            if has_children {
                let offset = (focus_x - i32::from(bar.x)).clamp(0, i32::from(bar.width)) as u16;
                let bottom = Rect::new(
                    bar.x + offset,
                    bar.y + bar_height - 1,
                    bar.width - offset,
                    1,
                );
                let tree =
                    (bar_height == 3).then(|| Rect::new(bottom.x, bottom.y - 1, bottom.width, 1));
                let focus = if self.active_idx == parent {
                    parent + 1
                } else {
                    self.active_idx
                };
                self.draw_row(buffer, bottom, parent + 1, end, focus, false, None, tree);
            }
        }
        cursor
    }

    ////////////////////////////////////////////////////////////////////////////
    // Moving between tabs, horizontal scroll updates

    fn select_tab(&mut self, tab_idx: usize) {
        if tab_idx < self.tabs.len() && tab_idx != self.active_idx {
            self.active_idx = tab_idx;
            self.tabs[tab_idx].set_style(TabStyle::Normal);
        }
    }

    pub(crate) fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.select_tab((self.active_idx + 1) % self.tabs.len());
        }
    }

    pub(crate) fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.select_tab((self.active_idx + self.tabs.len() - 1) % self.tabs.len());
        }
    }

    pub(crate) fn switch(&mut self, string: &str) {
        let mut next_idx = self.active_idx;
        for (tab_idx, tab) in self.tabs.iter().enumerate() {
            match tab.src {
                MsgSource::Serv { ref serv } => {
                    if serv.contains(string) {
                        next_idx = tab_idx;
                        break;
                    }
                }
                MsgSource::Chan { ref chan, .. } => {
                    // TODO: Case sensitive matching here is not ideal
                    if chan.display().contains(string) {
                        next_idx = tab_idx;
                        break;
                    }
                }
                MsgSource::User { ref nick, .. } => {
                    if nick.contains(string) {
                        next_idx = tab_idx;
                        break;
                    }
                }
            }
        }
        if next_idx != self.active_idx {
            self.select_tab(next_idx);
        }
    }

    fn move_tab_left(&mut self) {
        if self.active_idx == 0 {
            return;
        }
        if self.is_server_tab(self.active_idx) {
            // move all server tabs
            let (left, right) = self.server_tab_range(self.active_idx);
            if left > 0 {
                let mut insert_idx = left - 1;
                while insert_idx > 0 && !self.is_server_tab(insert_idx) {
                    insert_idx -= 1;
                }
                let to_move: Vec<Tab> = self.tabs.drain(left..right).collect();
                self.tabs.splice(insert_idx..insert_idx, to_move);
                self.active_idx = insert_idx;
            }
        } else if !self.is_server_tab(self.active_idx - 1) {
            let tab = self.tabs.remove(self.active_idx);
            self.tabs.insert(self.active_idx - 1, tab);
            let active_idx = self.active_idx - 1;
            self.active_idx = active_idx;
        }
    }

    fn move_tab_right(&mut self) {
        if self.tabs.is_empty() || self.active_idx == self.tabs.len() - 1 {
            return;
        }
        if self.is_server_tab(self.active_idx) {
            // move all server tabs
            let (left, right) = self.server_tab_range(self.active_idx);
            if right < self.tabs.len() {
                let right_next = self.server_tab_range(right).1;
                let insert_idx = right_next - (right - left);
                let to_move: Vec<Tab> = self.tabs.drain(left..right).collect();
                self.tabs.splice(insert_idx..insert_idx, to_move);
                self.active_idx = insert_idx;
            }
        } else if !self.is_server_tab(self.active_idx + 1) {
            let tab = self.tabs.remove(self.active_idx);
            self.tabs.insert(self.active_idx + 1, tab);
            let active_idx = self.active_idx + 1;
            self.active_idx = active_idx;
        }
    }

    fn go_to_tab(&mut self, c: char) {
        if self.tabs.is_empty() {
            return;
        }
        match c.to_digit(10) {
            Some(i) => {
                let new_tab_idx: usize = if i as usize > self.tabs.len() || i == 0 {
                    self.tabs.len() - 1
                } else {
                    i as usize - 1
                };
                self.select_tab(new_tab_idx);
            }
            None => {
                // multiple tabs can have same switch character so scan
                // forwards instead of starting from the first tab
                for i in 1..=self.tabs.len() {
                    let idx = (self.active_idx + i) % self.tabs.len();
                    if self.tabs[idx].switch == Some(c) {
                        self.select_tab(idx);
                        break;
                    }
                }
            }
        }
    }

    ////////////////////////////////////////////////////////////////////////////
    // Interfacing with tabs

    fn apply_to_target<F>(&mut self, target: &MsgTarget, can_create_tab: bool, f: &mut F)
    where
        F: FnMut(&mut Tab, bool),
    {
        // Creating a vector just to make borrow checker happy (I can't have a Vec<&mut Tab>)
        // I need to collect tabs here because of the "create if not exists" logic.
        // (see `target_idxs.is_empty()` below)
        let mut target_idxs: Vec<usize> = Vec::with_capacity(1);

        match *target {
            MsgTarget::Server { serv } => {
                for (tab_idx, tab) in self.tabs.iter().enumerate() {
                    if let MsgSource::Serv { serv: ref serv_ } = tab.src
                        && serv == serv_
                    {
                        target_idxs.push(tab_idx);
                        break;
                    }
                }
            }

            MsgTarget::Chan { serv, chan } => {
                for (tab_idx, tab) in self.tabs.iter().enumerate() {
                    if let MsgSource::Chan {
                        serv: ref serv_,
                        chan: ref chan_,
                    } = tab.src
                        && serv == serv_
                        && chan == chan_
                    {
                        target_idxs.push(tab_idx);
                        break;
                    }
                }
            }

            MsgTarget::User { serv, nick } => {
                for (tab_idx, tab) in self.tabs.iter().enumerate() {
                    if let MsgSource::User {
                        serv: ref serv_,
                        nick: ref nick_,
                    } = tab.src
                        && serv == serv_
                        && nick == nick_
                    {
                        target_idxs.push(tab_idx);
                        break;
                    }
                }
            }

            MsgTarget::AllServTabs { serv } => {
                for (tab_idx, tab) in self.tabs.iter().enumerate() {
                    if tab.src.serv_name() == serv {
                        target_idxs.push(tab_idx);
                    }
                }
            }

            MsgTarget::CurrentTab => {
                if !self.tabs.is_empty() {
                    target_idxs.push(self.active_idx);
                }
            }
        }

        // Create server/chan/user tab when necessary
        if target_idxs.is_empty()
            && can_create_tab
            && let Some(idx) = self.maybe_create_tab(target)
        {
            target_idxs.push(idx);
        }

        for tab_idx in target_idxs {
            f(&mut self.tabs[tab_idx], self.active_idx == tab_idx);
        }
    }

    fn maybe_create_tab(&mut self, target: &MsgTarget) -> Option<usize> {
        match *target {
            MsgTarget::Server { serv } | MsgTarget::AllServTabs { serv } => {
                self.new_server_tab(serv, None)
            }

            MsgTarget::Chan { serv, chan } => self.new_chan_tab(serv, chan),

            MsgTarget::User { serv, nick } => self.new_user_tab(serv, nick),

            _ => None,
        }
    }

    pub(crate) fn set_tab_style(&mut self, style: TabStyle, target: &MsgTarget) {
        let ignore = self
            .get_tab_config(
                target.serv_name().unwrap_or_default(),
                target.chan_or_user_name(),
            )
            .ignore
            .unwrap_or_default();
        self.apply_to_target(target, false, &mut |tab: &mut Tab, is_active: bool| {
            if (!ignore || style != TabStyle::JoinOrPart) && tab.style < style && !is_active {
                tab.set_style(style);
            }
        });
    }

    /// An error message coming from Tiny, probably because of a command error
    /// etc. Those are not timestamped and not logged.
    pub(crate) fn add_client_err_msg(&mut self, msg: &str, target: &MsgTarget) {
        self.apply_to_target(target, true, &mut |tab: &mut Tab, _| {
            tab.widget.add_client_err_msg(msg);
        });
    }

    /// A notify message coming from tiny, usually shows a response of a command
    /// e.g. "Notifications enabled".
    pub(crate) fn add_client_notify_msg(&mut self, msg: &str, target: &MsgTarget) {
        self.apply_to_target(target, false, &mut |tab: &mut Tab, _| {
            tab.widget.add_client_notify_msg(msg);
        });
    }

    /// A message from client, usually just to indidate progress, e.g.
    /// "Connecting...". Not timestamed and not logged.
    pub(crate) fn add_client_msg(&mut self, msg: &str, target: &MsgTarget) {
        self.apply_to_target(target, false, &mut |tab: &mut Tab, _| {
            tab.widget.add_client_msg(msg);
        });
    }

    /// privmsg is a message coming from a server or client. Shown with sender's
    /// nick/name and receive time and logged.
    pub(crate) fn add_privmsg(
        &mut self,
        sender: &str,
        msg: &str,
        ts: Tm,
        target: &MsgTarget,
        highlight: bool,
        is_action: bool,
    ) {
        let mut notifier = if let Some(serv) = target.serv_name() {
            self.get_tab_config(serv, target.chan_or_user_name())
                .notify
                .unwrap_or_default()
        } else {
            Notifier::default()
        };
        self.apply_to_target(target, true, &mut |tab: &mut Tab, _| {
            tab.widget
                .add_privmsg(sender, msg, Timestamp::from(ts), highlight, is_action);
            let nick = tab.widget.get_nick();
            if let Some(nick_) = nick {
                notifier.notify_privmsg(sender, msg, target, &nick_, highlight);
            }
        });
    }

    /// A message without any explicit sender info. Useful for e.g. in server
    /// and debug log tabs. Timestamped and logged.
    pub fn add_msg(&mut self, msg: &str, ts: Tm, target: &MsgTarget) {
        self.apply_to_target(target, true, &mut |tab: &mut Tab, _| {
            tab.widget.add_msg(msg, Timestamp::from(ts));
        });
    }

    /// Error messages related with the protocol - e.g. can't join a channel,
    /// nickname is in use etc. Timestamped and logged.
    pub(crate) fn add_err_msg(&mut self, msg: &str, ts: Tm, target: &MsgTarget) {
        self.apply_to_target(target, true, &mut |tab: &mut Tab, _| {
            tab.widget.add_err_msg(msg, Timestamp::from(ts));
        });
    }

    pub(crate) fn set_topic(&mut self, title: &str, ts: Tm, serv: &str, chan: &ChanNameRef) {
        let target = MsgTarget::Chan { serv, chan };
        self.apply_to_target(&target, false, &mut |tab: &mut Tab, _| {
            tab.widget.show_topic(title, Timestamp::from(ts));
        });
    }

    pub(crate) fn clear_nicks(&mut self, serv: &str) {
        let target = MsgTarget::AllServTabs { serv };
        self.apply_to_target(&target, false, &mut |tab: &mut Tab, _| {
            tab.widget.clear_nicks();
        });
    }

    pub(crate) fn add_nick(&mut self, nick: &str, ts: Option<Tm>, target: &MsgTarget) {
        let ignore = self
            .get_tab_config(
                target.serv_name().unwrap_or_default(),
                target.chan_or_user_name(),
            )
            .ignore
            .unwrap_or_default();

        self.apply_to_target(target, false, &mut |tab: &mut Tab, _| {
            tab.widget.join(nick, ts.map(Timestamp::from), ignore);
        });
    }

    pub(crate) fn remove_nick(&mut self, nick: &str, ts: Option<Tm>, target: &MsgTarget) {
        let ignore = self
            .get_tab_config(
                target.serv_name().unwrap_or_default(),
                target.chan_or_user_name(),
            )
            .ignore
            .unwrap_or_default();

        self.apply_to_target(target, false, &mut |tab: &mut Tab, _| {
            tab.widget.part(nick, ts.map(Timestamp::from), ignore);
        });
    }

    pub(crate) fn rename_nick(
        &mut self,
        old_nick: &str,
        new_nick: &str,
        ts: Tm,
        target: &MsgTarget,
    ) {
        self.apply_to_target(target, false, &mut |tab: &mut Tab, _| {
            tab.widget.nick(old_nick, new_nick, Timestamp::from(ts));
            // TODO: Does this actually rename the tab?
            tab.update_source(&|src: &mut MsgSource| {
                if let MsgSource::User { ref mut nick, .. } = *src {
                    nick.clear();
                    nick.push_str(new_nick);
                }
            });
        });
    }

    pub(crate) fn set_nick(&mut self, serv: &str, new_nick: &str) {
        let target = MsgTarget::AllServTabs { serv };
        self.apply_to_target(&target, false, &mut |tab: &mut Tab, _| {
            tab.widget.set_nick(new_nick.to_owned())
        });
    }

    pub(crate) fn clear(&mut self, target: &MsgTarget) {
        self.apply_to_target(target, false, &mut |tab: &mut Tab, _| tab.widget.clear());
    }

    // TODO: Maybe remove this and add a `create: bool` field to MsgTarget::User
    pub(crate) fn user_tab_exists(&self, serv_: &str, nick_: &str) -> bool {
        for tab in &self.tabs {
            if let MsgSource::User { ref serv, ref nick } = tab.src
                && serv_ == serv
                && nick_ == nick
            {
                return true;
            }
        }
        false
    }

    pub(crate) fn set_notifier(&mut self, notifier: Notifier, target: &MsgTarget) {
        if let Some(serv) = target.serv_name()
            && let Some(config) = self.tab_configs.get_mut(serv, target.chan_or_user_name())
        {
            config.notify = Some(notifier);
        }
    }

    pub(crate) fn show_notify_mode(&mut self, src: &MsgSource) {
        let notifier = self
            .get_tab_config(src.serv_name(), src.chan_name())
            .notify
            .unwrap_or_default();
        self.apply_to_target(&src.to_target(), false, &mut |tab: &mut Tab, _| {
            let msg = match notifier {
                Notifier::Off => "Notifications are off",
                Notifier::Mentions => "Notifications enabled for mentions",
                Notifier::Messages => "Notifications enabled for all messages",
            };
            tab.widget.add_client_notify_msg(msg);
        });
    }

    ////////////////////////////////////////////////////////////////////////////
    // Helpers

    fn find_serv_tab_idx(&self, serv_: &str) -> Option<usize> {
        for (tab_idx, tab) in self.tabs.iter().enumerate() {
            if let MsgSource::Serv { ref serv } = tab.src
                && serv_ == serv
            {
                return Some(tab_idx);
            }
        }
        None
    }

    fn find_chan_tab_idx(&self, serv_: &str, chan_: &ChanNameRef) -> Option<usize> {
        for (tab_idx, tab) in self.tabs.iter().enumerate() {
            if let MsgSource::Chan { ref serv, ref chan } = tab.src
                && serv_ == serv
                && chan_ == chan
            {
                return Some(tab_idx);
            }
        }
        None
    }

    fn find_user_tab_idx(&self, serv_: &str, nick_: &str) -> Option<usize> {
        for (tab_idx, tab) in self.tabs.iter().enumerate() {
            if let MsgSource::User { ref serv, ref nick } = tab.src
                && serv_ == serv
                && nick_ == nick
            {
                return Some(tab_idx);
            }
        }
        None
    }

    /// Index of the last tab with the given server name.
    fn find_last_serv_tab_idx(&self, serv: &str) -> Option<usize> {
        for (tab_idx, tab) in self.tabs.iter().enumerate().rev() {
            if tab.src.serv_name() == serv {
                return Some(tab_idx);
            }
        }
        None
    }

    fn is_server_tab(&self, idx: usize) -> bool {
        match self.tabs[idx].src {
            MsgSource::Serv { .. } => true,
            MsgSource::Chan { .. } | MsgSource::User { .. } => false,
        }
    }

    /// Given a tab index return range of tabs for the server of this tab.
    fn server_tab_range(&self, idx: usize) -> (usize, usize) {
        debug_assert!(idx < self.tabs.len());
        let mut left = idx;
        while !self.is_server_tab(left) {
            left -= 1;
        }
        let mut right = idx + 1;
        while right < self.tabs.len() && !self.is_server_tab(right) {
            right += 1;
        }
        (left, right)
    }
}
