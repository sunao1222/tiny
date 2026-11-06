#![cfg(unix)]

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

struct Pty {
    child: Child,
    master: File,
    slave: File,
    initial: libc::termios,
    dir: TempDir,
    output: Vec<u8>,
    screen: vt100::Parser,
}

impl Pty {
    fn new(editor: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.yml"),
            "servers: []\ndefaults:\n  nicks: [smoke]\n  realname: smoke\n  tls: false\n",
        )
        .unwrap();
        let mut master = -1;
        let mut slave = -1;
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut initial = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut initial) },
            0
        );
        let editor_path = dir.path().join("editor");
        if editor.is_none() {
            fs::write(&editor_path, "#!/bin/sh\ncp \"$1\" \"$EDITOR_CONTENT\"\nprintf 'EDITOR_READY\\n'\nIFS= read -r line\nprintf '%s' \"$line\" > \"$EDITOR_INPUT.tmp\"\nmv \"$EDITOR_INPUT.tmp\" \"$EDITOR_INPUT\"\n").unwrap();
            fs::set_permissions(&editor_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_tiny"));
        command
            .current_dir(dir.path())
            .arg("--config")
            .arg(dir.path().join("config.yml"))
            .env("TERM", "xterm-256color")
            .env(
                "EDITOR",
                editor
                    .map(str::to_owned)
                    .unwrap_or_else(|| editor_path.to_string_lossy().into_owned()),
            )
            .env("EDITOR_CONTENT", dir.path().join("editor-content"))
            .env("EDITOR_INPUT", dir.path().join("editor-input"))
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let mut pty = Self {
            child,
            master,
            slave,
            initial,
            dir,
            output: Vec::new(),
            screen: vt100::Parser::new(24, 100, 0),
        };
        pty.until(b"mentions");
        pty
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(self.slave.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
            0
        );
        self.screen.screen_mut().set_size(rows, cols);
    }

    fn read_some(&mut self, timeout: Duration) {
        let mut fd = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ret =
            unsafe { libc::poll(&mut fd, 1, timeout.as_millis().min(i32::MAX as u128) as i32) };
        assert!(ret >= 0, "PTY poll failed: {}", io::Error::last_os_error());
        if ret > 0 && fd.revents & libc::POLLIN != 0 {
            let mut buf = [0; 8192];
            match self.master.read(&mut buf) {
                Ok(n) => {
                    self.output.extend_from_slice(&buf[..n]);
                    self.screen.process(&buf[..n]);
                }
                Err(e) if e.raw_os_error() == Some(libc::EIO) => (),
                Err(e) => panic!("PTY read failed: {e}"),
            }
        }
    }

    fn until(&mut self, needle: &[u8]) {
        let end = Instant::now() + WAIT;
        while !self.output.windows(needle.len()).any(|w| w == needle) {
            assert!(
                Instant::now() < end,
                "timed out waiting for {:?}; output: {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.output)
            );
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "tiny exited {status}; output: {:?}",
                    String::from_utf8_lossy(&self.output)
                );
            }
            self.read_some(Duration::from_millis(30));
        }
    }

    fn until_screen(&mut self, needle: &str) {
        let end = Instant::now() + WAIT;
        while !self.screen.screen().contents().contains(needle) {
            assert!(
                Instant::now() < end,
                "timed out waiting for {needle:?} on screen: {:?}",
                self.screen.screen().contents()
            );
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "tiny exited {status}; output: {:?}",
                    String::from_utf8_lossy(&self.output)
                );
            }
            self.read_some(Duration::from_millis(30));
        }
    }

    fn until_screen_absent(&mut self, needle: &str) {
        let end = Instant::now() + WAIT;
        while self.screen.screen().contents().contains(needle) {
            assert!(
                Instant::now() < end,
                "timed out waiting for {needle:?} to disappear: {:?}",
                self.screen.screen().contents()
            );
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "tiny exited {status}; output: {:?}",
                    String::from_utf8_lossy(&self.output)
                );
            }
            self.read_some(Duration::from_millis(30));
        }
    }

    fn until_tab(&mut self, needle: &str) {
        let end = Instant::now() + WAIT;
        loop {
            let (rows, cols) = self.screen.screen().size();
            let bar = self
                .screen
                .screen()
                .rows(0, cols)
                .nth(usize::from(rows - 1))
                .unwrap();
            if bar.contains(needle) {
                return;
            }
            assert!(
                Instant::now() < end,
                "timed out waiting for tab {needle:?}: {bar:?}"
            );
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("tiny exited {status}");
            }
            self.read_some(Duration::from_millis(30));
        }
    }

    fn clear(&mut self) {
        self.output.clear();
    }

    fn quit(&mut self) {
        self.send(b"\x03\r");
        let end = Instant::now() + WAIT;
        loop {
            if self.child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < end, "tiny did not quit");
            self.read_some(Duration::from_millis(30));
        }
        while {
            let n = self.output.len();
            self.read_some(Duration::from_millis(10));
            self.output.len() > n
        } {}
        let mut final_mode: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut final_mode) },
            0
        );
        assert_eq!(
            final_mode.c_lflag, self.initial.c_lflag,
            "terminal local modes not restored"
        );
        assert_eq!(
            final_mode.c_iflag, self.initial.c_iflag,
            "terminal input modes not restored"
        );
        assert!(
            self.output.windows(6).any(|w| w == b"\x1b[?25h"),
            "cursor was not shown on exit"
        );
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_file(path: &std::path::Path) -> String {
    let end = Instant::now() + WAIT;
    loop {
        if let Ok(value) = fs::read_to_string(path) {
            return value;
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn split_escape_editor_and_paste() {
    let mut p = Pty::new(None);
    p.clear();
    p.send(b"abc\x1b");
    thread::sleep(Duration::from_millis(220));
    p.send(b"[DX\x18");
    p.until(b"EDITOR_READY");
    let exit = p
        .output
        .windows(8)
        .position(|w| w == b"\x1b[?1049l")
        .expect("alternate screen not exited");
    let ready = p
        .output
        .windows(12)
        .position(|w| w == b"EDITOR_READY")
        .unwrap();
    assert!(exit < ready);
    assert!(wait_file(&p.dir.path().join("editor-content")).contains("abXc"));
    p.clear();
    p.send(b"editor-input\n");
    assert_eq!(
        wait_file(&p.dir.path().join("editor-input")),
        "editor-input"
    );
    p.until(b"\x1b[?1049h");
    p.until(b"mentions");
    // A standalone Escape cancels the quit dialogue. Alt-f and F1 are unbound,
    // while the fragmented left arrow must move the insertion point.
    p.send(b"\x03");
    p.until(b"Really quit?");
    p.send(b"\x1b");
    thread::sleep(Duration::from_millis(300));
    p.send(b"pq\x1bf\x1b[");
    thread::sleep(Duration::from_millis(80));
    p.send(b"D\x1bO");
    thread::sleep(Duration::from_millis(80));
    p.send(b"PX");
    fs::remove_file(p.dir.path().join("editor-content")).unwrap();
    fs::remove_file(p.dir.path().join("editor-input")).unwrap();
    p.clear();
    p.send(b"\x18");
    p.until(b"EDITOR_READY");
    assert!(wait_file(&p.dir.path().join("editor-content")).contains("pXq"));
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.clear();
    p.until(b"\x1b[?1049h");
    // Clear the input, then verify single-line bracketed paste is editable input.
    fs::remove_file(p.dir.path().join("editor-content")).unwrap();
    p.clear();
    p.send(b"\x01\x0b\x1b[200~single paste\x1b[201~\x18");
    p.until(b"EDITOR_READY");
    assert!(wait_file(&p.dir.path().join("editor-content")).contains("single paste"));
    fs::remove_file(p.dir.path().join("editor-input")).unwrap();
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.clear();
    p.until(b"\x1b[?1049h");
    fs::remove_file(p.dir.path().join("editor-content")).unwrap();
    p.clear();
    p.send(b"\x01\x0b\x1b[200~first\nsecond\x1b[201~");
    p.until(b"EDITOR_READY");
    assert!(wait_file(&p.dir.path().join("editor-content")).contains("first\nsecond"));
    fs::remove_file(p.dir.path().join("editor-input")).unwrap();
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.clear();
    p.until(b"\x1b[?1049h");
    p.quit();
}

#[test]
fn consecutive_escapes_keep_following_input_in_order() {
    let mut p = Pty::new(None);
    p.send(b"z");
    p.until_screen("z");
    p.send(b"\x7f\x1b\x1ba");
    thread::sleep(Duration::from_millis(80));
    p.send(b"b\x18");
    p.until(b"EDITOR_READY");
    let contents = wait_file(&p.dir.path().join("editor-content"));
    assert!(contents.ends_with("\nb"), "unexpected input: {contents:?}");
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.until(b"\x1b[?1049h");
    p.quit();
}

#[test]
fn split_bracketed_paste_keeps_its_terminator_and_newlines() {
    let mut p = Pty::new(None);
    p.clear();
    p.send(b"\x1b");
    thread::sleep(Duration::from_millis(220));
    p.send(b"[200~first\nsecond");
    thread::sleep(Duration::from_millis(50));
    p.send(b"\x1b[201~");
    p.until(b"EDITOR_READY");
    assert!(wait_file(&p.dir.path().join("editor-content")).contains("first\nsecond"));
    p.clear();
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.until(b"\x1b[?1049h");
    p.quit();
}

#[test]
fn resize_interrupts_a_pending_escape_without_losing_following_keys() {
    let mut p = Pty::new(None);
    p.send(b"abc\x1b");
    thread::sleep(Duration::from_millis(80));
    p.clear();
    p.resize(20, 80);
    p.until(b"\x1b[2J");
    p.clear();
    p.send(b"[D\x18");
    p.until(b"EDITOR_READY");
    let contents = wait_file(&p.dir.path().join("editor-content"));
    assert!(contents.contains("abc[D"), "{contents:?}");
    p.clear();
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.until(b"\x1b[?1049h");
    p.quit();
}

#[test]
fn mouse_wheel_scrolls_with_split_and_complete_sequences() {
    let mut p = Pty::new(None);
    assert!(
        p.output
            .windows(b"\x1b[?1000h".len())
            .any(|bytes| bytes == b"\x1b[?1000h")
    );
    p.send(b"/help\r");
    p.until_screen("Displays this message");
    p.clear();
    p.resize(8, 80);
    p.until(b"\x1b[2J");
    p.until_screen("Displays this message");
    p.send(b"\x1b");
    thread::sleep(Duration::from_millis(220));
    p.send(b"[<64;10;5M");
    p.until_screen_absent("Displays this message");
    p.send(b"\x1b[<65;10;5M");
    p.until_screen("Displays this message");
    // RXVT and X10 encodings are used by terminals without SGR mouse mode.
    p.send(b"\x1b[96;10;5M");
    p.until_screen_absent("Displays this message");
    p.send(b"\x1b[97;10;5M");
    p.until_screen("Displays this message");
    p.send(b"\x1b[M\x60\xe9\x25");
    p.until_screen_absent("Displays this message");
    p.send(b"\x1b[M\x61\xe9\x25");
    p.until_screen("Displays this message");
    p.quit();
}

#[test]
fn editor_paste_uses_the_grapheme_cursor_byte_position() {
    let mut p = Pty::new(None);
    p.clear();
    p.send("Aか\u{3099}👩‍💻B".as_bytes());
    p.send(b"\x1b[D\x7f");
    p.send(b"\x1b[200~first\nsecond\x1b[201~");
    p.until(b"EDITOR_READY");
    let contents = wait_file(&p.dir.path().join("editor-content"));
    assert!(
        contents.contains("Aか\u{3099}first\nsecondB"),
        "{contents:?}"
    );
    p.clear();
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.until(b"\x1b[?1049h");
    p.quit();
}

#[test]
fn editor_launch_failure_restores_terminal() {
    let mut p = Pty::new(Some("/nonexistent/tiny-test-editor"));
    p.send(b"\x18");
    p.until(b"Error");
    p.quit();
}

#[test]
fn uppercase_and_pasted_key_bindings_execute_in_a_terminal() {
    let mut p = Pty::new(None);
    fs::write(
        p.dir.path().join("config.yml"),
        "servers: []\ndefaults:\n  nicks: [smoke]\n  realname: smoke\n  tls: false\nkey_map:\n  A:\n    command: clear\n  x:\n    command: clear\n",
    )
    .unwrap();
    p.send(b"/reload\r");
    p.until_screen("Reloaded config file.");
    p.send(b"A");
    p.until_screen_absent("Reloaded config file.");
    p.send(b"/help\r");
    p.until_screen("Displays this message");
    p.send(b"\x1b[200~x\x1b[201~");
    p.until_screen_absent("Displays this message");
    p.quit();
}

#[test]
fn legacy_control_punctuation_bindings_work_in_a_terminal() {
    let mut p = Pty::new(None);
    fs::write(
        p.dir.path().join("config.yml"),
        "servers: []\ndefaults:\n  nicks: [smoke]\n  realname: smoke\n  tls: false\nkey_map:\n  ctrl_/:\n    input: a\n  ctrl_\\:\n    input: b\n  ctrl_]:\n    input: c\n",
    )
    .unwrap();
    p.send(b"/reload\r");
    p.until_screen("Reloaded config file.");
    p.send(b"\x1f\x1c\x1d\x18");
    p.until(b"EDITOR_READY");
    let contents = wait_file(&p.dir.path().join("editor-content"));
    assert!(contents.ends_with("\nabc"), "{contents:?}");
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.until(b"\x1b[?1049h");
    p.quit();
}

#[test]
fn function_and_modified_arrow_bindings_work_in_a_terminal() {
    let mut p = Pty::new(None);
    let mut config = String::from(
        "servers: []\ndefaults:\n  nicks: [smoke]\n  realname: smoke\n  tls: false\nkey_map:\n",
    );
    for (index, ch) in ('a'..='l').enumerate() {
        config.push_str(&format!("  f{}:\n    input: {ch}\n", index + 1));
    }
    config.push_str("  alt_left:\n    input: m\n  ctrl_left:\n    input: n\n  shift_left:\n    input: o\n  alt_f1:\n    input: p\n  ctrl_f2:\n    input: q\n  shift_f12:\n    input: r\n");
    fs::write(p.dir.path().join("config.yml"), config).unwrap();
    p.send(b"/reload\r");
    p.until_screen("Reloaded config file.");
    p.send(b"\x1bOP\x1bOQ\x1bOR\x1bOS\x1b[15~\x1b[17~\x1b[18~\x1b[19~\x1b[20~\x1b[21~\x1b[23~\x1b[24~\x1b[1;3D\x1b[1;5D\x1b[1;2D\x1b[1;3P\x1b[1;5Q\x1b[24;2~\x1b[1;1:3P\x1b[1;1:2P\x18");
    p.until(b"EDITOR_READY");
    let contents = wait_file(&p.dir.path().join("editor-content"));
    assert!(contents.ends_with("\nabcdefghijklmnopqra"), "{contents:?}");
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.until(b"\x1b[?1049h");
    p.quit();
}

#[test]
fn enhanced_keyboard_repeat_does_not_treat_release_as_input() {
    let mut p = Pty::new(None);
    p.send(b"\x1b[97;1:1u\x1b[97;1:2u\x1b[97;1:3u\x1b[98;1u\x1b[1;1:3Dc\x1b[1;1:2Dd\x1b[97:65;2:1u\x18");
    p.until(b"EDITOR_READY");
    let contents = wait_file(&p.dir.path().join("editor-content"));
    assert!(contents.ends_with("\naabdAc"), "{contents:?}");
    p.send(b"done\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "done");
    p.until(b"\x1b[?1049h");
    p.quit();
}

fn read_line(stream: &mut TcpStream, pending: &mut Vec<u8>) -> String {
    let end = Instant::now() + WAIT;
    loop {
        if let Some(i) = pending.iter().position(|&b| b == b'\n') {
            return String::from_utf8(pending.drain(..=i).collect()).unwrap();
        }
        assert!(Instant::now() < end, "timed out reading IRC line");
        let mut buf = [0; 4096];
        match stream.read(&mut buf) {
            Ok(0) => panic!("IRC connection closed"),
            Ok(n) => pending.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                ()
            }
            Err(e) => panic!("IRC read: {e}"),
        }
    }
}
fn expect_line(stream: &mut TcpStream, pending: &mut Vec<u8>, expected: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for IRC line {expected:?}"
        );
        if read_line(stream, pending) == expected {
            break;
        }
    }
}

#[test]
fn local_irc_and_editor_suspension() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut p = Pty::new(None);
    p.send(
        format!(
            "/connect 127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        )
        .as_bytes(),
    );
    p.send(b"\r");
    let end = Instant::now() + WAIT;
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < end => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(e) => panic!("IRC accept: {e}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut pending = Vec::new();
    let mut nick = false;
    let mut user = false;
    while !nick || !user {
        let line = read_line(&mut stream, &mut pending);
        nick |= line == "NICK smoke\r\n";
        user |= line.starts_with("USER ");
    }
    stream.write_all(b":local 001 smoke :welcome\r\n").unwrap();
    p.send(b"\x0e");
    p.clear();
    p.until_screen("welcome");
    p.send(b"/join #demo\r");
    expect_line(&mut stream, &mut pending, "JOIN #demo\r\n");
    stream
        .write_all(
            ":smoke!u@localhost JOIN :#demo\r\n:alice!u@localhost PRIVMSG #demo :hello 日本\r\n"
                .as_bytes(),
        )
        .unwrap();
    p.clear();
    p.until_tab("#demo");
    let screen = p.screen.screen();
    let (rows, cols) = screen.size();
    let tree_col = (0..cols)
        .find(|&col| screen.cell(rows - 2, col).unwrap().contents() == "│")
        .expect("missing tree connector above channel row");
    assert!(tree_col + 1 < cols, "tree connector has no room for icons");
    assert_eq!(screen.cell(rows - 3, tree_col + 1).unwrap().contents(), "●");
    assert_eq!(screen.cell(rows - 1, tree_col + 1).unwrap().contents(), "·");
    p.send(b"\x0e");
    p.clear();
    p.until_screen("hello 日本");
    p.send(b"reply\r");
    expect_line(&mut stream, &mut pending, "PRIVMSG #demo :reply\r\n");
    p.send(b"\x18");
    p.clear();
    p.until(b"EDITOR_READY");
    p.clear();
    stream
        .write_all(b":alice!u@localhost PRIVMSG #demo :during-editor\r\n")
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    p.read_some(Duration::from_millis(30));
    assert!(
        !p.output
            .windows(b"during-editor".len())
            .any(|w| w == b"during-editor"),
        "IRC painted over editor"
    );
    p.send(b"finished\n");
    assert_eq!(wait_file(&p.dir.path().join("editor-input")), "finished");
    p.until(b"\x1b[?1049h");
    p.until_screen("during-editor");
    p.quit();
}
