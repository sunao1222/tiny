//! Launch the user's editor after the input task has relinquished the terminal.
//! The editor runs on a blocking thread while IRC updates continue in the runtime.
use tokio::sync::oneshot;

use std::io::Write;
use std::process::Command;

#[derive(Debug)]
pub(crate) struct Error {
    /// Original contents of the text field passed to `editor::run`. This should be used to restore
    /// text field contents in case of an error as otherwise the input will be lost.
    pub(crate) text_field_contents: String,
    /// Original UTF-8 byte position, used to restore the input on error.
    pub(crate) cursor: usize,
    /// The actual error
    pub(crate) kind: ErrorKind,
}

#[derive(Debug)]
pub(crate) enum ErrorKind {
    Io(::std::io::Error),
    Var(::std::env::VarError),
}

impl Error {
    fn new(text_field_contents: String, cursor: usize, kind: ErrorKind) -> Error {
        Error {
            text_field_contents,
            cursor,
            kind,
        }
    }
}

impl From<::std::io::Error> for ErrorKind {
    fn from(err: ::std::io::Error) -> ErrorKind {
        ErrorKind::Io(err)
    }
}

impl From<::std::env::VarError> for ErrorKind {
    fn from(err: ::std::env::VarError) -> ErrorKind {
        ErrorKind::Var(err)
    }
}

pub(crate) type Result<A> = ::std::result::Result<A, Error>;

pub(crate) type ResultReceiver = oneshot::Receiver<Result<Vec<String>>>;

/// `cursor` is a UTF-8 byte position at a grapheme boundary.
pub(crate) fn run(
    text_field_contents: String,
    cursor: usize,
    pasted_text: &str,
) -> Result<ResultReceiver> {
    let editor = match std::env::var("EDITOR") {
        Ok(editor) => editor,
        Err(err) => {
            return Err(Error::new(text_field_contents, cursor, err.into()));
        }
    };

    let (mut tmp_file, tmp_file_path) = match tempfile::NamedTempFile::new() {
        Ok(tmp_file) => tmp_file.into_parts(),
        Err(err) => {
            return Err(Error::new(text_field_contents, cursor, err.into()));
        }
    };

    let (before_paste, after_paste) = text_field_contents.split_at(cursor);
    // Editors expect a scalar position, while the input widget stores byte offsets.
    let scalar_cursor = before_paste.chars().count();

    let write_ret = write!(
        tmp_file,
        "# You pasted a multi-line message. When you close the editor final version of\n\
         # this file will be sent (ignoring these lines). Delete contents to abort the\n\
         # paste.\n\
         {}{}{}",
        before_paste,
        pasted_text.replace('\r', "\n"),
        after_paste,
    )
    .and_then(|_| tmp_file.sync_all());

    drop(tmp_file);

    if let Err(err) = write_ret {
        return Err(Error::new(text_field_contents, cursor, err.into()));
    }

    let (snd_editor_ret, rcv_editor_ret) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new(&editor);

        // If the editor is (n)vim or emacs, pass parameters to move the cursor to its location in
        // `text_field_contents`
        match editor.as_ref() {
            "vim" | "nvim" => {
                if scalar_cursor == 0 {
                    cmd.arg("-c").arg("normal! 3j");
                } else {
                    cmd.arg("-c").arg(format!("normal! 3j{scalar_cursor}l"));
                }
            }
            "emacs" => {
                cmd.arg(format!("+4:{}", scalar_cursor + 1));
            }
            _ => {}
        }

        let ret = cmd.arg(&tmp_file_path).status();
        let ret = match ret {
            Err(io_err) => {
                let _ = snd_editor_ret.send(Err(Error::new(
                    text_field_contents,
                    cursor,
                    io_err.into(),
                )));
                return;
            }
            Ok(ret) => ret,
        };

        if !ret.success() {
            // Assume aborted
            let _ = snd_editor_ret.send(Ok(vec![]));
            return;
        }

        let file_contents = {
            match std::fs::read_to_string(tmp_file_path) {
                Ok(file_contents) => file_contents,
                Err(io_err) => {
                    let _ = snd_editor_ret.send(Err(Error::new(
                        text_field_contents,
                        cursor,
                        io_err.into(),
                    )));
                    return;
                }
            }
        };

        let mut filtered_lines = vec![];
        for s in file_contents.lines() {
            // Ignore if the char is '#'. To actually send a `#` add space.
            // For empty lines, send " ".
            let first_char = s.chars().next();
            if first_char == Some('#') {
                // skip this line
                continue;
            } else if s.is_empty() {
                filtered_lines.push(" ".to_owned());
            } else {
                filtered_lines.push(s.to_owned());
            }
        }

        let _ = snd_editor_ret.send(Ok(filtered_lines));
    });

    Ok(rcv_editor_ret)
}
