/* ncdurs - a ratatui backend that writes classic 8-colour SGR codes
 *
 * crossterm always spells colours in its 256-colour form ("38;5;3", "48;5;6"),
 * while ncurses uses the classic codes ("33", "46"). Most terminals map the
 * first 16 entries of the 256-colour table onto their own palette, so the two
 * look the same - but some themes render "38;5;3" as a fixed colour, which
 * makes the dark theme look different from the C version. Emitting the classic
 * codes keeps the two byte-for-byte comparable and lets the palette win.
 *
 * Everything except `draw` is a thin wrapper around crossterm.
 */

use std::io::{self, Write};

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::crossterm::cursor::{Hide, MoveTo, Show, position};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{self, Clear};
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier, Style};

pub struct AnsiBackend<W: Write> {
    out: W,
}

impl<W: Write> AnsiBackend<W> {
    pub fn new(out: W) -> AnsiBackend<W> {
        AnsiBackend { out }
    }
}

impl<W: Write> Write for AnsiBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl<W: Write> Backend for AnsiBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut buf = Vec::new();
        let mut style = Style::default();
        for (x, y, cell) in content {
            // Position every cell explicitly: that keeps wide characters and
            // partial redraws correct without tracking the cursor ourselves.
            write!(buf, "\x1b[{};{}H", y + 1, x + 1)?;
            if cell.style() != style {
                write_sgr(&mut buf, cell.style())?;
                style = cell.style();
            }
            buf.extend_from_slice(cell.symbol().as_bytes());
        }
        self.out.write_all(&buf)?;
        self.out.flush()
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(self.out, Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(self.out, Show)
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        // crossterm asks the terminal, so it does not use our writer (which is
        // stdout anyway).
        let (x, y) = position()?;
        Ok(Position::new(x, y))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        execute!(self.out, MoveTo(position.x, position.y))
    }

    fn clear(&mut self) -> io::Result<()> {
        execute!(self.out, Clear(terminal::ClearType::All))
    }

    fn clear_region(&mut self, region: ClearType) -> io::Result<()> {
        execute!(self.out, Clear(clear_type(region)))
    }

    fn size(&self) -> io::Result<Size> {
        let (width, height) = terminal::size()?;
        Ok(Size::new(width, height))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let size = terminal::window_size()?;
        Ok(WindowSize {
            columns_rows: Size::new(size.columns, size.rows),
            pixels: Size::new(size.width, size.height),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.out)
    }
}

/// crossterm carries its own ClearType; map ratatui's onto it.
fn clear_type(region: ClearType) -> terminal::ClearType {
    match region {
        ClearType::All => terminal::ClearType::All,
        ClearType::AfterCursor => terminal::ClearType::FromCursorDown,
        ClearType::BeforeCursor => terminal::ClearType::FromCursorUp,
        ClearType::CurrentLine => terminal::ClearType::CurrentLine,
        ClearType::UntilNewLine => terminal::ClearType::UntilNewLine,
    }
}

/// Writes a full SGR sequence for `style`. A reset comes first: the terminal
/// may carry anything from the previous cell, and the C version resets just as
/// often (ncurses emits `\033[0m` before most attributes).
fn write_sgr(out: &mut Vec<u8>, style: Style) -> io::Result<()> {
    out.extend_from_slice(b"\x1b[0m");
    if let Some(fg) = style.fg {
        write_colour(out, fg, true)?;
    }
    if let Some(bg) = style.bg {
        write_colour(out, bg, false)?;
    }
    for (modifier, code) in [
        (Modifier::BOLD, 1),
        (Modifier::DIM, 2),
        (Modifier::ITALIC, 3),
        (Modifier::UNDERLINED, 4),
        (Modifier::REVERSED, 7),
    ] {
        if style.add_modifier.contains(modifier) {
            write!(out, "\x1b[{code}m")?;
        }
    }
    Ok(())
}

/// Writes one colour: the classic code when there is one - that is what ncurses
/// emits and it follows the terminal's palette - otherwise the 256-colour or
/// true-colour form.
fn write_colour(out: &mut Vec<u8>, colour: Color, foreground: bool) -> io::Result<()> {
    if let Some((fg, bg)) = ansi_colour(colour) {
        let code = if foreground { fg } else { bg };
        return write!(out, "\x1b[{code}m");
    }
    let lead = if foreground { 38 } else { 48 };
    match colour {
        Color::Indexed(n) => write!(out, "\x1b[{lead};5;{n}m"),
        Color::Rgb(r, g, b) => write!(out, "\x1b[{lead};2;{r};{g};{b}m"),
        // Reset and the like never reach here (they are None on Style).
        _ => Ok(()),
    }
}

/// The classic SGR pair for a colour. `Color::Gray` is ANSI 7 and
/// `Color::White` is ANSI 15, matching COLOR_WHITE in the C version's palette.
fn ansi_colour(colour: Color) -> Option<(u8, u8)> {
    Some(match colour {
        Color::Black => (30, 40),
        Color::Red => (31, 41),
        Color::Green => (32, 42),
        Color::Yellow => (33, 43),
        Color::Blue => (34, 44),
        Color::Magenta => (35, 45),
        Color::Cyan => (36, 46),
        Color::Gray => (37, 47),
        Color::DarkGray => (90, 100),
        Color::LightRed => (91, 101),
        Color::LightGreen => (92, 102),
        Color::LightYellow => (93, 103),
        Color::LightBlue => (94, 104),
        Color::LightMagenta => (95, 105),
        Color::LightCyan => (96, 106),
        Color::White => (97, 107),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sgr(style: Style) -> String {
        let mut out = Vec::new();
        write_sgr(&mut out, style).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// The exact codes the C version emits for the dark theme.
    #[test]
    fn classic_codes_for_the_theme_colours() {
        // UIC_HD: BLACK,CYAN,0
        assert_eq!(
            sgr(Style::default().fg(Color::Black).bg(Color::Cyan)),
            "\x1b[0m\x1b[30m\x1b[46m"
        );
        // UIC_NUM: YELLOW,-,B
        assert_eq!(
            sgr(Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)),
            "\x1b[0m\x1b[33m\x1b[1m"
        );
        // UIC_SEL: WHITE,GREEN,B (COLOR_WHITE is ANSI 7 -> Color::Gray here)
        assert_eq!(
            sgr(Style::default()
                .fg(Color::Gray)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)),
            "\x1b[0m\x1b[37m\x1b[42m\x1b[1m"
        );
        // UIC_GRAPH: MAGENTA,-,0
        assert_eq!(sgr(Style::default().fg(Color::Magenta)), "\x1b[0m\x1b[35m");
        // UIC_DIR: BLUE,-,B
        assert_eq!(
            sgr(Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD)),
            "\x1b[0m\x1b[34m\x1b[1m"
        );
        // UIC_FLAG: RED,-,0
        assert_eq!(sgr(Style::default().fg(Color::Red)), "\x1b[0m\x1b[31m");
        // The off theme only uses attributes.
        assert_eq!(
            sgr(Style::default().add_modifier(Modifier::REVERSED)),
            "\x1b[0m\x1b[7m"
        );
        assert_eq!(sgr(Style::default()), "\x1b[0m");
    }

    /// Colours without a classic spelling keep the extended form.
    #[test]
    fn extended_colours_are_preserved() {
        assert_eq!(
            sgr(Style::default().fg(Color::Indexed(200))),
            "\x1b[0m\x1b[38;5;200m"
        );
        assert_eq!(
            sgr(Style::default().bg(Color::Rgb(1, 2, 3))),
            "\x1b[0m\x1b[48;2;1;2;3m"
        );
    }
}
