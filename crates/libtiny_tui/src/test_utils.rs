use std::panic::Location;

use ratatui::buffer::Buffer;
use unicode_width::UnicodeWidthStr;

pub fn buffer_str(buf: &Buffer, w: u16, h: u16) -> String {
    let w = usize::from(w);
    let h = usize::from(h);

    let mut ret = String::with_capacity(w * h);

    for y in 0..h {
        let mut x = 0;
        while x < w {
            let symbol = buf[(buf.area.x + x as u16, buf.area.y + y as u16)].symbol();
            ret.push_str(symbol);
            // A wide symbol already occupies the following continuation cells.
            x += UnicodeWidthStr::width(symbol).max(1);
        }
        if y != h - 1 {
            ret.push('\n');
        }
    }

    ret
}

pub fn expect_screen(
    screen: &str,
    front_buffer: &Buffer,
    w: u16,
    h: u16,
    caller: &'static Location<'static>,
) {
    let mut screen_filtered = String::with_capacity(screen.len());

    let mut in_screen = false;
    for c in screen.chars() {
        if in_screen {
            if c == '|' {
                screen_filtered.push('\n');
                in_screen = false;
            } else {
                screen_filtered.push(c);
            }
        } else if c == '|' {
            in_screen = true;
        }
    }
    let _ = screen_filtered.pop(); // pop the last '\n'

    let found = buffer_str(front_buffer, w, h);

    let mut line = String::new();
    for _ in 0..w {
        line.push('-');
    }

    if screen_filtered != found {
        panic!(
            "Unexpected screen\n\
            Expected:\n\
            {line}\n\
            {screen_filtered}\n\
            {line}\n\
            Found:\n\
            {line}\n\
            {found}\n\
            {line}\n\
            Called by: {caller}\n"
        );
    }
}
