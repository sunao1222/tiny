use std::fs::{File, OpenOptions};
use std::io::{self, Write};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::queue;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};

enum SessionTerminal {
    Real(Terminal<CrosstermBackend<File>>),
    Test(Terminal<TestBackend>),
}

#[derive(Default)]
struct Modes {
    raw: bool,
    alternate: bool,
    paste: bool,
    mouse: bool,
    cursor_hidden: bool,
}

pub(crate) struct TerminalSession {
    terminal: SessionTerminal,
    modes: Modes,
    suspended: bool,
}

impl TerminalSession {
    pub(crate) fn new() -> io::Result<Self> {
        let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        let terminal = Terminal::new(CrosstermBackend::new(tty))?;
        let mut session = Self {
            terminal: SessionTerminal::Real(terminal),
            modes: Modes::default(),
            suspended: true,
        };
        session.resume()?;
        Ok(session)
    }

    pub(crate) fn new_test(width: u16, height: u16) -> io::Result<Self> {
        let terminal = Terminal::new(TestBackend::new(width, height))
            .map_err(|never| -> io::Error { match never {} })?;
        Ok(Self {
            terminal: SessionTerminal::Test(terminal),
            modes: Modes::default(),
            suspended: false,
        })
    }

    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut Frame)) -> io::Result<()> {
        if self.suspended {
            return Ok(());
        }
        match &mut self.terminal {
            SessionTerminal::Real(terminal) => {
                // Ratatui may hide the cursor even if drawing subsequently fails.
                self.modes.cursor_hidden = true;
                terminal.draw(render)?;
            }
            SessionTerminal::Test(terminal) => {
                terminal
                    .draw(render)
                    .map_err(|never| -> io::Error { match never {} })?;
            }
        }
        Ok(())
    }

    pub(crate) fn size(&self) -> io::Result<(u16, u16)> {
        let size = match &self.terminal {
            SessionTerminal::Real(terminal) => terminal.size()?,
            SessionTerminal::Test(terminal) => terminal
                .size()
                .map_err(|never| -> io::Error { match never {} })?,
        };
        Ok((size.width, size.height))
    }

    pub(crate) fn set_size(&mut self, width: u16, height: u16) -> io::Result<()> {
        match &mut self.terminal {
            SessionTerminal::Test(terminal) => {
                terminal.backend_mut().resize(width, height);
                terminal
                    .resize(Rect::new(0, 0, width, height))
                    .map_err(|never| -> io::Error { match never {} })?;
                Ok(())
            }
            SessionTerminal::Real(terminal) => terminal.resize(Rect::new(0, 0, width, height)),
        }
    }

    pub(crate) fn suspend(&mut self) -> io::Result<()> {
        if matches!(self.terminal, SessionTerminal::Test(_)) {
            return Ok(());
        }
        self.suspended = true;
        self.restore_modes()
    }

    pub(crate) fn resume(&mut self) -> io::Result<(u16, u16)> {
        if matches!(self.terminal, SessionTerminal::Test(_)) || !self.suspended {
            return self.size();
        }
        let result = self.enable_modes().and_then(|()| match &mut self.terminal {
            SessionTerminal::Real(terminal) => {
                let size = terminal.size()?;
                terminal.resize(Rect::new(0, 0, size.width, size.height))?;
                terminal.backend_mut().flush()?;
                Ok((size.width, size.height))
            }
            SessionTerminal::Test(terminal) => {
                let size = terminal
                    .size()
                    .map_err(|never| -> io::Error { match never {} })?;
                Ok((size.width, size.height))
            }
        });
        match result {
            Ok(size) => {
                self.suspended = false;
                Ok(size)
            }
            Err(error) => match self.restore_modes() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(io::Error::new(
                    error.kind(),
                    format!("{error}; terminal restoration failed: {cleanup}"),
                )),
            },
        }
    }

    pub(crate) fn front_buffer(&mut self, render: impl FnOnce(&mut Frame)) -> io::Result<Buffer> {
        match &mut self.terminal {
            SessionTerminal::Test(terminal) => Ok(terminal.backend().buffer().clone()),
            SessionTerminal::Real(terminal) if !self.suspended => {
                self.modes.cursor_hidden = true;
                Ok(terminal.draw(render)?.buffer.clone())
            }
            SessionTerminal::Real(_) => Err(io::Error::other("terminal is suspended")),
        }
    }

    fn enable_modes(&mut self) -> io::Result<()> {
        let SessionTerminal::Real(terminal) = &mut self.terminal else {
            return Ok(());
        };
        enable_raw_mode()?;
        self.modes.raw = true;
        // A failed write may have emitted part of a command, so cleanup must
        // attempt its inverse even if the write itself reports an error.
        self.modes.alternate = true;
        queue!(terminal.backend_mut(), EnterAlternateScreen)?;
        terminal.backend_mut().flush()?;
        self.modes.cursor_hidden = true;
        queue!(terminal.backend_mut(), Hide)?;
        terminal.backend_mut().flush()?;
        self.modes.paste = true;
        queue!(terminal.backend_mut(), EnableBracketedPaste)?;
        terminal.backend_mut().flush()?;
        self.modes.mouse = true;
        queue!(terminal.backend_mut(), EnableMouseCapture)?;
        terminal.backend_mut().flush()?;
        Ok(())
    }

    fn restore_modes(&mut self) -> io::Result<()> {
        let SessionTerminal::Real(terminal) = &mut self.terminal else {
            return Ok(());
        };
        let mut first_error = None;
        macro_rules! restore {
            ($flag:ident, $command:expr) => {
                if self.modes.$flag {
                    match queue!(terminal.backend_mut(), $command)
                        .and_then(|()| terminal.backend_mut().flush())
                    {
                        Ok(()) => self.modes.$flag = false,
                        Err(error) => {
                            if first_error.is_none() {
                                first_error = Some(error);
                            }
                        }
                    }
                }
            };
        }
        restore!(mouse, DisableMouseCapture);
        restore!(paste, DisableBracketedPaste);
        restore!(cursor_hidden, Show);
        restore!(alternate, LeaveAlternateScreen);
        if let Err(error) = terminal.backend_mut().flush() {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        if self.modes.raw {
            match disable_raw_mode() {
                Ok(()) => self.modes.raw = false,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore_modes();
    }
}
