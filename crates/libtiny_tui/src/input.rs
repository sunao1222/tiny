//! Ordered terminal input, including escape sequences split across reads.
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEvent, MouseEventKind,
};
use termwiz::escape::{
    Action, CSI,
    csi::{MouseButton, MouseReport},
    parser::Parser,
};
use termwiz::input::{InputEvent as ParsedEvent, InputParser, KeyCode as ParsedKey, Modifiers};
use tokio::sync::mpsc;

const QUEUE_CAPACITY: usize = 64;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const RETRY_INTERVAL: Duration = Duration::from_millis(5);
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_ESCAPE_BYTES: usize = 32;
const PASTE_START: &[u8] = b"\x1b[200~";

/// Owns the only thread on which terminal input is polled and read.
pub(crate) struct InputReader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<StoppedInput>>,
}

pub(crate) struct ReaderState {
    normalizer: EscapeNormalizer,
    decoder: RawKeyDecoder,
}

impl ReaderState {
    fn new() -> Self {
        Self {
            normalizer: EscapeNormalizer::new(),
            decoder: RawKeyDecoder::default(),
        }
    }
}

pub(crate) struct StoppedInput {
    pub(crate) pending: Vec<io::Result<Event>>,
    pub(crate) state: ReaderState,
}

impl InputReader {
    pub(crate) fn start(state: Option<ReaderState>) -> (Self, mpsc::Receiver<io::Result<Event>>) {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let state = state.unwrap_or_else(ReaderState::new);
            let mut terminal = match TerminalInput::new() {
                Ok(terminal) => terminal,
                Err(error) => {
                    let _ = sender.try_send(Err(error));
                    return StoppedInput {
                        pending: Vec::new(),
                        state,
                    };
                }
            };
            input_worker(worker_stop, sender, state, |timeout| terminal.read(timeout))
        });
        (
            Self {
                stop,
                thread: Some(thread),
            },
            receiver,
        )
    }

    /// Pending events follow everything already sent to the receiver. A partial
    /// sequence is retained separately and resumed after those events.
    pub(crate) fn stop(mut self) -> io::Result<StoppedInput> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| io::Error::other("terminal input thread panicked"))
    }
}

impl Drop for InputReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

enum ReadInput {
    Byte(u8),
    Resize(u16, u16),
    Idle,
}

/// Reads all terminal input on one thread so bytes cannot overtake buffered events.
struct TerminalInput {
    tty: File,
    resized: Arc<AtomicBool>,
    signal: signal_hook::SigId,
}

impl TerminalInput {
    fn new() -> io::Result<Self> {
        let tty = if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
            let fd = unsafe { libc::dup(libc::STDIN_FILENO) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            unsafe { File::from_raw_fd(fd) }
        } else {
            OpenOptions::new().read(true).open("/dev/tty")?
        };
        let resized = Arc::new(AtomicBool::new(false));
        let signal =
            signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&resized))?;
        Ok(Self {
            tty,
            resized,
            signal,
        })
    }

    fn read(&mut self, timeout: Duration) -> io::Result<ReadInput> {
        if self.resized.swap(false, Ordering::AcqRel) {
            let (width, height) = crossterm::terminal::size()?;
            return Ok(ReadInput::Resize(width, height));
        }
        let mut pollfd = libc::pollfd {
            fd: self.tty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut pollfd,
                1,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if self.resized.swap(false, Ordering::AcqRel) {
            let (width, height) = crossterm::terminal::size()?;
            return Ok(ReadInput::Resize(width, height));
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted {
                Ok(ReadInput::Idle)
            } else {
                Err(error)
            };
        }
        if result == 0 {
            return Ok(ReadInput::Idle);
        }
        if pollfd.revents & libc::POLLIN != 0 {
            let mut byte = [0];
            self.tty.read_exact(&mut byte)?;
            return Ok(ReadInput::Byte(byte[0]));
        }
        if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("terminal input disconnected"));
        }
        Ok(ReadInput::Idle)
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.signal);
    }
}

fn input_worker(
    stop: Arc<AtomicBool>,
    sender: mpsc::Sender<io::Result<Event>>,
    mut state: ReaderState,
    mut read: impl FnMut(Duration) -> io::Result<ReadInput>,
) -> StoppedInput {
    let mut pending = VecDeque::new();
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Some(event) = pending.pop_front() {
            match sender.try_send(event) {
                Ok(()) => continue,
                Err(mpsc::error::TrySendError::Full(event)) => {
                    pending.push_front(event);
                    thread::sleep(RETRY_INTERVAL);
                    continue;
                }
                Err(mpsc::error::TrySendError::Closed(event)) => {
                    pending.push_front(event);
                    break;
                }
            }
        }
        pending.extend(state.normalizer.expire(Instant::now()).into_iter().map(Ok));
        if !pending.is_empty() {
            continue;
        }
        let next = read(POLL_INTERVAL).and_then(|input| match input {
            ReadInput::Byte(byte) => {
                let now = Instant::now();
                pending.extend(state.normalizer.expire(now).into_iter().map(Ok));
                state.normalizer.refresh(now);
                if state.normalizer.x10_pending() {
                    pending.extend(
                        state
                            .normalizer
                            .push_x10_byte(byte, now)
                            .into_iter()
                            .map(Ok),
                    );
                    Ok(None)
                } else {
                    state.decoder.push(byte, state.normalizer.pasting)
                }
            }
            ReadInput::Resize(width, height) => {
                pending.extend(state.normalizer.flush().into_iter().map(Ok));
                Ok(Some(Event::Resize(width, height)))
            }
            ReadInput::Idle => Ok(None),
        });
        match next {
            Ok(Some(event)) if !state.normalizer.waiting() && !is_escape(&event) => {
                pending.push_back(Ok(event));
            }
            Ok(Some(event)) => pending.extend(
                state
                    .normalizer
                    .push(event, Instant::now())
                    .into_iter()
                    .map(Ok),
            ),
            Ok(None) => {}
            Err(error) => {
                pending.extend(state.normalizer.flush().into_iter().map(Ok));
                pending.push_back(Err(error));
                while let Some(event) = pending.pop_front() {
                    if stop.load(Ordering::Acquire) {
                        pending.push_front(event);
                        break;
                    }
                    match sender.try_send(event) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(event)) => {
                            pending.push_front(event);
                            thread::sleep(RETRY_INTERVAL);
                        }
                        Err(mpsc::error::TrySendError::Closed(event)) => {
                            pending.push_front(event);
                            break;
                        }
                    }
                }
                break;
            }
        }
    }
    StoppedInput {
        pending: pending.into_iter().collect(),
        state,
    }
}

#[derive(Default)]
struct RawKeyDecoder {
    utf8: [u8; 4],
    len: usize,
    expected: usize,
}

impl RawKeyDecoder {
    fn push(&mut self, byte: u8, pasting: bool) -> io::Result<Option<Event>> {
        if self.len != 0 {
            if byte & 0xc0 != 0x80 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid UTF-8 in terminal input",
                ));
            }
            self.utf8[self.len] = byte;
            self.len += 1;
            if self.len < self.expected {
                return Ok(None);
            }
            let text = std::str::from_utf8(&self.utf8[..self.len])
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            let ch = text.chars().next().unwrap();
            self.len = 0;
            return Ok(Some(character_event(ch)));
        }
        if byte >= 0x80 {
            self.expected = match byte {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid UTF-8 in terminal input",
                    ));
                }
            };
            self.utf8[0] = byte;
            self.len = 1;
            return Ok(None);
        }
        let (code, modifiers) = match byte {
            b'\n' if pasting => (KeyCode::Char('\n'), KeyModifiers::NONE),
            b'\r' => (KeyCode::Enter, KeyModifiers::NONE),
            b'\t' => (KeyCode::Tab, KeyModifiers::NONE),
            0x1b => (KeyCode::Esc, KeyModifiers::NONE),
            0x7f => (KeyCode::Backspace, KeyModifiers::NONE),
            0 => (KeyCode::Char(' '), KeyModifiers::CONTROL),
            1..=26 => (
                KeyCode::Char((byte - 1 + b'a') as char),
                KeyModifiers::CONTROL,
            ),
            28..=31 => (
                KeyCode::Char((byte - 28 + b'4') as char),
                KeyModifiers::CONTROL,
            ),
            _ => return Ok(Some(character_event(byte as char))),
        };
        Ok(Some(Event::Key(KeyEvent::new(code, modifiers))))
    }
}

fn character_event(ch: char) -> Event {
    let modifiers = if ch.is_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    Event::Key(KeyEvent::new(KeyCode::Char(ch), modifiers))
}

/// Holds the original events until a sequence is known, so failed parses lose nothing.
struct EscapeNormalizer {
    parser: InputParser,
    original: Vec<Event>,
    bytes: Vec<u8>,
    deadline: Option<Instant>,
    pasting: bool,
}

impl EscapeNormalizer {
    fn new() -> Self {
        Self {
            parser: InputParser::new(),
            original: Vec::new(),
            bytes: Vec::new(),
            deadline: None,
            pasting: false,
        }
    }

    fn waiting(&self) -> bool {
        !self.original.is_empty()
    }

    fn refresh(&mut self, now: Instant) {
        if self.waiting() {
            self.deadline = Some(now + ESCAPE_TIMEOUT);
        }
    }

    fn reset(&mut self) {
        self.parser = InputParser::new();
        self.bytes.clear();
        self.deadline = None;
        self.pasting = false;
    }

    fn flush(&mut self) -> Vec<Event> {
        let events = std::mem::take(&mut self.original);
        self.reset();
        events
    }

    fn expire(&mut self, now: Instant) -> Vec<Event> {
        if !self.pasting && self.deadline.is_some_and(|deadline| now >= deadline) {
            self.flush()
        } else {
            Vec::new()
        }
    }
    fn x10_pending(&self) -> bool {
        !self.pasting && self.bytes.starts_with(b"\x1b[M") && self.bytes.len() < 6
    }

    fn push_x10_byte(&mut self, byte: u8, now: Instant) -> Vec<Event> {
        self.bytes.push(byte);
        self.original.push(character_event(char::from(byte)));
        self.deadline = Some(now + ESCAPE_TIMEOUT);
        if self.bytes.len() < 6 {
            return Vec::new();
        }
        let Some(button) = self.bytes[3].checked_sub(32) else {
            return self.flush();
        };
        let column = self.bytes[4].saturating_sub(33).into();
        let row = self.bytes[5].saturating_sub(33).into();
        let event = mouse_wheel_event(button, column, row);
        self.original.clear();
        self.reset();
        event.into_iter().collect()
    }

    fn push(&mut self, event: Event, now: Instant) -> Vec<Event> {
        let mut output = self.expire(now);
        if self.original.is_empty() {
            if is_escape(&event) {
                self.original.push(event);
                self.bytes.push(0x1b);
                self.deadline = Some(now + ESCAPE_TIMEOUT);
                self.parser.parse(b"\x1b", |_| {}, true);
            } else {
                output.push(event);
            }
            return output;
        }

        if !self.pasting && is_escape(&event) {
            output.extend(self.flush());
            output.extend(self.push(event, now));
            return output;
        }
        let Some(bytes) = event_bytes(&event, self.pasting) else {
            output.extend(self.flush());
            output.push(event);
            return output;
        };
        self.original.push(event);
        for byte in bytes.data[..bytes.len].iter().copied() {
            self.bytes.push(byte);
            self.deadline = Some(now + ESCAPE_TIMEOUT);
            if !self.pasting && self.bytes.starts_with(b"\x1b[<") {
                if self.bytes.len() > MAX_ESCAPE_BYTES {
                    output.extend(self.flush());
                    return output;
                }
                if matches!(byte, b'M' | b'm') {
                    match Parser::new().parse_first(&self.bytes) {
                        Some((
                            Action::CSI(CSI::Mouse(MouseReport::SGR1006 {
                                x,
                                y,
                                button,
                                modifiers,
                            })),
                            len,
                        )) if len == self.bytes.len() => {
                            let kind = match button {
                                MouseButton::Button4Press => Some(MouseEventKind::ScrollUp),
                                MouseButton::Button5Press => Some(MouseEventKind::ScrollDown),
                                _ => None,
                            };
                            self.original.clear();
                            self.reset();
                            if let Some(kind) = kind {
                                output.push(Event::Mouse(MouseEvent {
                                    kind,
                                    column: x.saturating_sub(1),
                                    row: y.saturating_sub(1),
                                    modifiers: key_modifiers(&modifiers),
                                }));
                            }
                            return output;
                        }
                        _ => {
                            output.extend(self.flush());
                            return output;
                        }
                    }
                }
                if byte == b'<' || byte == b';' || byte.is_ascii_digit() {
                    continue;
                }
                output.extend(self.flush());
                return output;
            }
            if !self.pasting && self.bytes == b"\x1b[M" {
                continue;
            }
            // Hold numeric CSI parameters until the final byte: RXVT mouse
            // reports and Kitty keyboard event types are not in termwiz's keymap.
            let numeric_csi = !self.pasting
                && self.bytes.starts_with(b"\x1b[")
                && self.bytes.get(2).is_some_and(u8::is_ascii_digit);
            if numeric_csi && self.bytes.len() > MAX_ESCAPE_BYTES {
                output.extend(self.flush());
                return output;
            }
            if numeric_csi && (byte.is_ascii_digit() || matches!(byte, b';' | b':')) {
                continue;
            }
            if numeric_csi && byte == b'M' {
                if let Some(mouse) = parse_rxvt_mouse(&self.bytes) {
                    self.original.clear();
                    self.reset();
                    output.extend(mouse);
                } else {
                    output.extend(self.flush());
                }
                return output;
            }
            if numeric_csi && self.bytes.contains(&b':') {
                if let Some(event) = parse_enhanced_key(&self.bytes) {
                    self.original.clear();
                    self.reset();
                    output.push(event);
                } else {
                    output.extend(self.flush());
                }
                return output;
            }
            let mut parsed = None;
            let mut multiple = false;
            let parse_bytes = if numeric_csi {
                &self.bytes[2..]
            } else {
                std::slice::from_ref(&byte)
            };
            self.parser.parse(
                parse_bytes,
                |event| {
                    if parsed.is_some() {
                        multiple = true;
                    } else {
                        parsed = Some(event);
                    }
                },
                true,
            );
            if self.bytes == PASTE_START {
                self.pasting = true;
            }
            if !self.pasting && self.bytes.len() > MAX_ESCAPE_BYTES {
                output.extend(self.flush());
                return output;
            }
            if multiple {
                output.extend(self.flush());
                return output;
            }
            if let Some(parsed) = parsed {
                match parsed {
                    ParsedEvent::Paste(text) if self.pasting => {
                        self.original.clear();
                        self.reset();
                        output.push(Event::Paste(text));
                        return output;
                    }
                    ParsedEvent::Key(key) if !self.pasting => {
                        // An unknown CSI/SS3 must not become Alt-[ or Alt-O.
                        let introducer = match self.bytes.get(1) {
                            Some(b'[') => Some('['),
                            Some(b'O') => Some('O'),
                            _ => None,
                        };
                        if let Some(event) = parsed_key(key) {
                            let fallback = matches!(event, Event::Key(KeyEvent { code: KeyCode::Char(c), modifiers, .. }) if Some(c) == introducer && modifiers.contains(KeyModifiers::ALT))
                                && !self.bytes.ends_with(b"u");
                            if !fallback {
                                self.original.clear();
                                self.reset();
                                output.push(event);
                                return output;
                            }
                        }
                    }
                    _ => {}
                }
                output.extend(self.flush());
                return output;
            }
        }
        output
    }
}

fn mouse_wheel_event(button: u8, column: u16, row: u16) -> Option<Event> {
    if button & 0xe0 != 0x40 {
        return None;
    }
    let kind = match button & 3 {
        0 => MouseEventKind::ScrollUp,
        1 => MouseEventKind::ScrollDown,
        _ => return None,
    };
    let mut modifiers = KeyModifiers::NONE;
    for (bit, modifier) in [
        (4, KeyModifiers::SHIFT),
        (8, KeyModifiers::ALT),
        (16, KeyModifiers::CONTROL),
    ] {
        if button & bit != 0 {
            modifiers |= modifier;
        }
    }
    Some(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    }))
}

fn parse_rxvt_mouse(bytes: &[u8]) -> Option<Option<Event>> {
    let text = std::str::from_utf8(bytes.get(2..bytes.len().checked_sub(1)?)?).ok()?;
    let mut fields = text.split(';');
    let button = fields.next()?.parse::<u8>().ok()?.checked_sub(32)?;
    let column = fields.next()?.parse::<u16>().ok()?.saturating_sub(1);
    let row = fields.next()?.parse::<u16>().ok()?.saturating_sub(1);
    if fields.next().is_some() {
        return None;
    }
    Some(mouse_wheel_event(button, column, row))
}

fn parse_enhanced_key(bytes: &[u8]) -> Option<Event> {
    let (&final_byte, body) = bytes.get(2..)?.split_last()?;
    let mut fields = std::str::from_utf8(body).ok()?.split(';');
    let mut codepoints = fields.next()?.split(':');
    let codepoint = codepoints.next()?.parse::<u32>().ok()?;
    let alternate = codepoints
        .next()
        .and_then(|value| value.parse::<u32>().ok());
    let mut modifier_fields = fields.next().unwrap_or("1").split(':');
    let mask = modifier_fields
        .next()?
        .parse::<u8>()
        .ok()?
        .saturating_sub(1);
    let kind = match modifier_fields.next().unwrap_or("1").parse::<u8>().ok()? {
        2 => KeyEventKind::Repeat,
        3 => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    };
    let mut modifiers = KeyModifiers::NONE;
    for (bit, modifier) in [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
        (8, KeyModifiers::SUPER),
        (16, KeyModifiers::HYPER),
        (32, KeyModifiers::META),
    ] {
        if mask & bit != 0 {
            modifiers |= modifier;
        }
    }
    let mut state = KeyEventState::NONE;
    if mask & 64 != 0 {
        state |= KeyEventState::CAPS_LOCK;
    }
    if mask & 128 != 0 {
        state |= KeyEventState::NUM_LOCK;
    }

    let mut code = match final_byte {
        b'u' => match codepoint {
            27 | 57344 => KeyCode::Esc,
            13 | 57345 | 57414 => KeyCode::Enter,
            9 | 57346 => KeyCode::Tab,
            127 | 57347 => KeyCode::Backspace,
            57348 | 57425 => KeyCode::Insert,
            57349 | 57426 => KeyCode::Delete,
            57350 | 57417 => KeyCode::Left,
            57351 | 57418 => KeyCode::Right,
            57352 | 57419 => KeyCode::Up,
            57353 | 57420 => KeyCode::Down,
            57354 | 57421 => KeyCode::PageUp,
            57355 | 57422 => KeyCode::PageDown,
            57356 | 57423 => KeyCode::Home,
            57357 | 57424 => KeyCode::End,
            57364..=57398 => KeyCode::F((codepoint - 57363) as u8),
            57399..=57408 => KeyCode::Char((b'0' + (codepoint - 57399) as u8) as char),
            57409 => KeyCode::Char('.'),
            57410 => KeyCode::Char('/'),
            57411 => KeyCode::Char('*'),
            57412 => KeyCode::Char('-'),
            57413 => KeyCode::Char('+'),
            57415 => KeyCode::Char('='),
            57416 => KeyCode::Char(','),
            _ => KeyCode::Char(char::from_u32(codepoint)?),
        },
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'F' => KeyCode::End,
        b'H' => KeyCode::Home,
        b'P'..=b'S' => KeyCode::F(final_byte - b'P' + 1),
        b'~' => match codepoint {
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=15 => KeyCode::F((codepoint - 10) as u8),
            17..=21 => KeyCode::F((codepoint - 11) as u8),
            23..=26 => KeyCode::F((codepoint - 12) as u8),
            28..=29 => KeyCode::F((codepoint - 15) as u8),
            31..=34 => KeyCode::F((codepoint - 17) as u8),
            _ => return None,
        },
        _ => return None,
    };
    if final_byte == b'u' && modifiers.contains(KeyModifiers::SHIFT) {
        if let Some(ch) = alternate.and_then(char::from_u32) {
            code = KeyCode::Char(ch);
            modifiers.remove(KeyModifiers::SHIFT);
        }
    }
    Some(Event::Key(KeyEvent::new_with_kind_and_state(
        code, modifiers, kind, state,
    )))
}

fn is_escape(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        })
    )
}

/// Recover the original bytes from a decoded character or control key.
struct KeyBytes {
    data: [u8; 5],
    len: usize,
}

fn event_bytes(event: &Event, pasting: bool) -> Option<KeyBytes> {
    let Event::Key(mut key) = *event else {
        return None;
    };
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let mut data = [0; 5];
    let offset = if pasting && key.modifiers.contains(KeyModifiers::ALT) {
        key.modifiers.remove(KeyModifiers::ALT);
        data[0] = 0x1b;
        1
    } else {
        0
    };
    let len = match key.code {
        KeyCode::Char(c)
            if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT =>
        {
            c.encode_utf8(&mut data[offset..]).len()
        }
        KeyCode::Char(c) if pasting && key.modifiers == KeyModifiers::CONTROL => {
            data[offset] = match c {
                'a'..='z' => c as u8 - b'a' + 1,
                'A'..='Z' => c as u8 - b'A' + 1,
                '4'..='7' => c as u8 - b'4' + 28,
                '@' | ' ' => 0,
                '[' => 27,
                '\\' => 28,
                ']' => 29,
                '^' => 30,
                '_' => 31,
                '?' => 127,
                _ => return None,
            };
            1
        }
        KeyCode::Enter if pasting && key.modifiers == KeyModifiers::NONE => {
            data[offset] = b'\r';
            1
        }
        KeyCode::Tab if pasting && key.modifiers == KeyModifiers::NONE => {
            data[offset] = b'\t';
            1
        }
        KeyCode::Backspace if pasting && key.modifiers == KeyModifiers::NONE => {
            data[offset] = 0x7f;
            1
        }
        KeyCode::Esc if pasting && key.modifiers == KeyModifiers::NONE => {
            data[offset] = 0x1b;
            1
        }
        _ => return None,
    };
    Some(KeyBytes {
        data,
        len: offset + len,
    })
}

fn parsed_key(key: termwiz::input::KeyEvent) -> Option<Event> {
    let code = match key.key {
        ParsedKey::Char(c) => KeyCode::Char(c),
        ParsedKey::Escape => KeyCode::Esc,
        ParsedKey::Backspace => KeyCode::Backspace,
        ParsedKey::Tab => KeyCode::Tab,
        ParsedKey::Enter => KeyCode::Enter,
        ParsedKey::LeftArrow | ParsedKey::ApplicationLeftArrow => KeyCode::Left,
        ParsedKey::RightArrow | ParsedKey::ApplicationRightArrow => KeyCode::Right,
        ParsedKey::UpArrow | ParsedKey::ApplicationUpArrow => KeyCode::Up,
        ParsedKey::DownArrow | ParsedKey::ApplicationDownArrow => KeyCode::Down,
        ParsedKey::Home | ParsedKey::KeyPadHome => KeyCode::Home,
        ParsedKey::End | ParsedKey::KeyPadEnd => KeyCode::End,
        ParsedKey::PageUp | ParsedKey::KeyPadPageUp => KeyCode::PageUp,
        ParsedKey::PageDown | ParsedKey::KeyPadPageDown => KeyCode::PageDown,
        ParsedKey::Insert => KeyCode::Insert,
        ParsedKey::Delete => KeyCode::Delete,
        ParsedKey::Function(n) => KeyCode::F(n),
        _ => return None,
    };
    Some(Event::Key(KeyEvent::new(
        code,
        key_modifiers(&key.modifiers),
    )))
}

fn key_modifiers(value: &Modifiers) -> KeyModifiers {
    let mut modifiers = KeyModifiers::NONE;
    if value.contains(Modifiers::SHIFT) {
        modifiers |= KeyModifiers::SHIFT;
    }
    if value.contains(Modifiers::ALT) {
        modifiers |= KeyModifiers::ALT;
    }
    if value.contains(Modifiers::CTRL) {
        modifiers |= KeyModifiers::CONTROL;
    }
    if value.contains(Modifiers::SUPER) {
        modifiers |= KeyModifiers::SUPER;
    }
    modifiers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn chars(normalizer: &mut EscapeNormalizer, text: &str, now: Instant) -> Vec<Event> {
        text.chars()
            .flat_map(|c| normalizer.push(key(KeyCode::Char(c)), now))
            .collect()
    }

    #[test]
    fn split_arrow_and_other_keys() {
        let now = Instant::now();
        let mut normalizer = EscapeNormalizer::new();
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert_eq!(
            chars(&mut normalizer, "[D", now + Duration::from_millis(220)),
            vec![key(KeyCode::Left)]
        );
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert_eq!(
            chars(&mut normalizer, "x", now),
            vec![Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::ALT
            ))]
        );
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert_eq!(chars(&mut normalizer, "OP", now), vec![key(KeyCode::F(1))]);
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert_eq!(
            chars(&mut normalizer, "[1;6D", now),
            vec![Event::Key(KeyEvent::new(
                KeyCode::Left,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))]
        );
    }

    #[test]
    fn split_mouse_wheel_is_delivered_as_mouse_input() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let now = Instant::now();
        let mut normalizer = EscapeNormalizer::new();
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert_eq!(
            chars(&mut normalizer, "[<64;10;5M", now),
            vec![Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 9,
                row: 4,
                modifiers: KeyModifiers::NONE,
            })]
        );
    }

    #[test]
    fn timeout_and_unrelated_events_preserve_order() {
        let now = Instant::now();
        let mut normalizer = EscapeNormalizer::new();
        let esc = key(KeyCode::Esc);
        assert!(normalizer.push(esc.clone(), now).is_empty());
        assert!(
            normalizer
                .expire(now + Duration::from_millis(249))
                .is_empty()
        );
        assert_eq!(
            normalizer.expire(now + Duration::from_millis(250)),
            vec![esc.clone()]
        );
        assert!(normalizer.push(esc.clone(), now).is_empty());
        assert!(chars(&mut normalizer, "[", now).is_empty());
        let enter = key(KeyCode::Enter);
        assert_eq!(
            normalizer.push(enter.clone(), now),
            vec![esc.clone(), key(KeyCode::Char('[')), enter]
        );
        assert!(normalizer.push(esc.clone(), now).is_empty());
        assert_eq!(normalizer.push(esc.clone(), now), vec![esc.clone()]);
        assert_eq!(normalizer.flush(), vec![esc]);
    }

    #[test]
    fn fragmented_paste_round_trips_controls_and_unicode() {
        let now = Instant::now();
        let mut normalizer = EscapeNormalizer::new();
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert!(chars(&mut normalizer, "[200~hello 日本", now).is_empty());
        assert!(normalizer.push(key(KeyCode::Enter), now).is_empty());
        assert!(normalizer.push(key(KeyCode::Tab), now).is_empty());
        assert!(normalizer.push(key(KeyCode::Backspace), now).is_empty());
        assert!(
            normalizer
                .push(
                    Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                    now
                )
                .is_empty()
        );
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert_eq!(
            chars(&mut normalizer, "[201~", now),
            vec![Event::Paste("hello 日本\r\t\u{7f}\u{3}".into())]
        );
    }

    #[test]
    fn unknown_and_long_sequences_preserve_original_events() {
        let now = Instant::now();
        let mut normalizer = EscapeNormalizer::new();
        let mut events = vec![key(KeyCode::Esc)];
        events.extend(
            format!("[{}z", "9".repeat(40))
                .chars()
                .map(|c| key(KeyCode::Char(c))),
        );
        let mut result = Vec::new();
        for event in events.iter().cloned() {
            result.extend(normalizer.push(event, now));
        }
        result.extend(normalizer.flush());
        assert_eq!(result, events);
    }

    #[test]
    fn long_paste_and_embedded_alt_character() {
        let now = Instant::now();
        let mut normalizer = EscapeNormalizer::new();
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert!(chars(&mut normalizer, "[200~", now).is_empty());
        let text = "x".repeat(80);
        assert!(chars(&mut normalizer, &text, now).is_empty());
        assert!(
            normalizer
                .push(
                    Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT)),
                    now
                )
                .is_empty()
        );
        assert!(normalizer.push(key(KeyCode::Esc), now).is_empty());
        assert_eq!(
            chars(&mut normalizer, "[201~", now),
            vec![Event::Paste(format!("{text}\u{1b}q"))]
        );
    }

    #[test]
    fn input_error_follows_pending_escape() {
        let (sender, mut receiver) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let mut calls = 0;
        let remaining = input_worker(stop, sender, ReaderState::new(), |_| {
            calls += 1;
            if calls == 1 {
                Ok(ReadInput::Byte(0x1b))
            } else {
                Err(io::Error::new(io::ErrorKind::InvalidData, "bad input"))
            }
        });
        assert!(remaining.pending.is_empty());
        assert_eq!(receiver.try_recv().unwrap().unwrap(), key(KeyCode::Esc));
        let error = receiver.try_recv().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "bad input");
    }

    #[test]
    fn resize_follows_the_pending_escape_without_losing_input() {
        let (sender, mut receiver) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let mut input = VecDeque::from([
            ReadInput::Byte(0x1b),
            ReadInput::Byte(b'['),
            ReadInput::Resize(80, 24),
        ]);
        input_worker(stop, sender, ReaderState::new(), |_| {
            input
                .pop_front()
                .ok_or_else(|| io::Error::other("finished"))
        });
        assert_eq!(receiver.try_recv().unwrap().unwrap(), key(KeyCode::Esc));
        assert_eq!(
            receiver.try_recv().unwrap().unwrap(),
            key(KeyCode::Char('['))
        );
        assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::Resize(80, 24));
        assert!(receiver.try_recv().unwrap().is_err());
    }

    #[test]
    fn stop_retains_an_incomplete_escape_for_resume() {
        let (sender, _receiver) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (polled_tx, polled_rx) = std::sync::mpsc::channel();
        let thread = thread::spawn(move || {
            let mut sent = false;
            input_worker(worker_stop, sender, ReaderState::new(), move |_| {
                if !sent {
                    sent = true;
                    polled_tx.send(()).unwrap();
                    Ok(ReadInput::Byte(0x1b))
                } else {
                    thread::sleep(Duration::from_millis(1));
                    Ok(ReadInput::Idle)
                }
            })
        });
        polled_rx.recv().unwrap();
        let reader = InputReader {
            stop,
            thread: Some(thread),
        };
        let mut stopped = reader.stop().unwrap();
        assert!(stopped.pending.is_empty());
        assert_eq!(
            stopped
                .state
                .normalizer
                .expire(Instant::now() + ESCAPE_TIMEOUT),
            vec![key(KeyCode::Esc)]
        );
    }

    #[test]
    fn stop_retains_events_after_a_full_queue() {
        let (sender, mut receiver) = mpsc::channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (polled_tx, polled_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let mut events = b"ab".iter().copied();
            input_worker(worker_stop, sender, ReaderState::new(), move |_| {
                let event = events.next();
                if event.is_some() {
                    polled_tx.send(()).unwrap();
                }
                Ok(event.map_or(ReadInput::Idle, ReadInput::Byte))
            })
        });
        polled_rx.recv().unwrap();
        polled_rx.recv().unwrap();
        stop.store(true, Ordering::Release);
        let remaining = worker.join().unwrap();
        assert_eq!(
            receiver.try_recv().unwrap().unwrap(),
            key(KeyCode::Char('a'))
        );
        assert_eq!(
            remaining
                .pending
                .into_iter()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            vec![key(KeyCode::Char('b'))]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn partial_utf8_paste_survives_reader_stop_and_resume() {
        let (sender, mut first_receiver) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (polled_tx, polled_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let mut input = VecDeque::from([ReadInput::Byte(0x1b)]);
            input.extend(b"[200~first\n\xe6".iter().copied().map(ReadInput::Byte));
            input_worker(worker_stop, sender, ReaderState::new(), move |_| {
                let next = input.pop_front().unwrap_or(ReadInput::Idle);
                if matches!(next, ReadInput::Byte(0xe6)) {
                    polled_tx.send(()).unwrap();
                }
                if matches!(next, ReadInput::Idle) {
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(next)
            })
        });
        polled_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        stop.store(true, Ordering::Release);
        let stopped = worker.join().unwrap();
        assert!(stopped.pending.is_empty());
        assert!(first_receiver.try_recv().is_err());

        // An editor may run longer than the Escape timeout.
        thread::sleep(Duration::from_millis(300));
        let (sender, mut receiver) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut input = b"\x97\xa5\x1b[201~".iter().copied();
            input_worker(worker_stop, sender, stopped.state, move |_| {
                match input.next() {
                    Some(byte) => Ok(ReadInput::Byte(byte)),
                    None => {
                        thread::sleep(Duration::from_millis(1));
                        Ok(ReadInput::Idle)
                    }
                }
            })
        });
        let event = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(event, Event::Paste("first\n日".into()));
        stop.store(true, Ordering::Release);
        worker.join().unwrap();
    }
}
