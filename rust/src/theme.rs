/* ncdurs - colour themes, mirroring UI_COLORS in src/util.h:36-53 and the
 * {fg, bg, attrs} table in src/util.c:262-278 of ncdu 1.22.
 *
 *   NAME        OFF         DARK                 DARK-BG
 *   DEFAULT     -,-,0       -,-,0                WHITE,BLACK,0
 *   BOX_TITLE   -,-,B       BLUE,-,B             BLUE,BLACK,B
 *   HD          -,-,R       BLACK,CYAN,0         BLACK,CYAN,0
 *   SEL         -,-,R       WHITE,GREEN,B        WHITE,GREEN,B
 *   NUM         -,-,0       YELLOW,-,B           YELLOW,BLACK,B
 *   NUM_HD      -,-,R       YELLOW,CYAN,B        YELLOW,CYAN,B
 *   NUM_SEL     -,-,R       YELLOW,GREEN,B       YELLOW,GREEN,B
 *   KEY         -,-,B       YELLOW,-,B           YELLOW,BLACK,B
 *   KEY_HD      -,-,B|R     YELLOW,CYAN,B        YELLOW,CYAN,B
 *   DIR         -,-,0       BLUE,-,B             BLUE,BLACK,B
 *   DIR_SEL     -,-,R       BLUE,GREEN,B         BLUE,GREEN,B
 *   FLAG        -,-,0       RED,-,0              RED,BLACK,0
 *   FLAG_SEL    -,-,R       RED,GREEN,0          RED,GREEN,0
 *   GRAPH       -,-,0       MAGENTA,-,0          MAGENTA,BLACK,0
 *   GRAPH_SEL   -,-,R       MAGENTA,GREEN,0      MAGENTA,GREEN,0
 *
 * "-" is the terminal default (COLOR__ / -1 in the C version), B is A_BOLD and
 * R is A_REVERSE. The `off` theme is what you get without `--color`, which is
 * also the C version's default.
 */

use ratatui::style::{Color, Modifier, Style};

/// The scheme selected by `--color` (uic_theme in the C version, plus an
/// extra `auto` that the C version does not have).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// Pick `dark` when the terminal can show colours, `off` otherwise.
    #[default]
    Auto,
    Off,
    Dark,
    DarkBg,
}

impl Theme {
    /// `--color auto|off|dark|dark-bg`. Returns None for an unknown value so
    /// the caller can report it the way the C version does.
    pub fn parse(s: &str) -> Option<Theme> {
        match s {
            "auto" => Some(Theme::Auto),
            "off" => Some(Theme::Off),
            "dark" => Some(Theme::Dark),
            "dark-bg" => Some(Theme::DarkBg),
            _ => None,
        }
    }

    /// The resolved styles. One field per slot in the C version's UI_COLORS.
    pub fn colors(self) -> Colors {
        match self {
            Theme::Auto => {
                if colors_supported() {
                    Theme::Dark.colors()
                } else {
                    Theme::Off.colors()
                }
            }
            Theme::Off => Colors {
                default: plain(),
                box_title: plain().add_modifier(Modifier::BOLD),
                hd: plain().add_modifier(Modifier::REVERSED),
                sel: plain().add_modifier(Modifier::REVERSED),
                num: plain(),
                num_hd: plain().add_modifier(Modifier::REVERSED),
                num_sel: plain().add_modifier(Modifier::REVERSED),
                key: plain().add_modifier(Modifier::BOLD),
                key_hd: plain().add_modifier(Modifier::BOLD | Modifier::REVERSED),
                dir: plain(),
                dir_sel: plain().add_modifier(Modifier::REVERSED),
                flag: plain(),
                flag_sel: plain().add_modifier(Modifier::REVERSED),
                graph: plain(),
                graph_sel: plain().add_modifier(Modifier::REVERSED),
            },
            Theme::Dark => Colors {
                default: plain(),
                box_title: Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                hd: Style::default().fg(Color::Black).bg(Color::Cyan),
                sel: Style::default()
                    .fg(Color::Gray)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                num: Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                num_hd: Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                num_sel: Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                key: Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                key_hd: Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                dir: Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                dir_sel: Style::default()
                    .fg(Color::Blue)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                flag: Style::default().fg(Color::Red),
                flag_sel: Style::default().fg(Color::Red).bg(Color::Green),
                graph: Style::default().fg(Color::Magenta),
                graph_sel: Style::default().fg(Color::Magenta).bg(Color::Green),
            },
            Theme::DarkBg => Colors {
                default: Style::default().fg(Color::Gray).bg(Color::Black),
                box_title: Style::default()
                    .fg(Color::Blue)
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD),
                hd: Style::default().fg(Color::Black).bg(Color::Cyan),
                sel: Style::default()
                    .fg(Color::Gray)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                num: Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD),
                num_hd: Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                num_sel: Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                key: Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD),
                key_hd: Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                dir: Style::default()
                    .fg(Color::Blue)
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD),
                dir_sel: Style::default()
                    .fg(Color::Blue)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                flag: Style::default().fg(Color::Red).bg(Color::Black),
                flag_sel: Style::default().fg(Color::Red).bg(Color::Green),
                graph: Style::default().fg(Color::Magenta).bg(Color::Black),
                graph_sel: Style::default().fg(Color::Magenta).bg(Color::Green),
            },
        }
    }
}

/// `Style::default()` - no foreground/background, no attributes: whatever the
/// terminal uses (the C version's COLOR__).
fn plain() -> Style {
    Style::default()
}

/// Whether the terminal is likely to render colours. The `off` theme works
/// everywhere (it only uses A_REVERSE/A_BOLD), while `dark` relies on colour
/// pairs for its header, footer and selected row - so `auto` falls back to
/// `off` when the terminal looks colourless.
pub fn colors_supported() -> bool {
    supports_colors(
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("TERM").ok().as_deref(),
    )
}

fn supports_colors(no_color: bool, term: Option<&str>) -> bool {
    if no_color {
        // https://no-color.org
        return false;
    }
    match term {
        Some(t) => !t.is_empty() && t != "dumb",
        // No TERM at all (e.g. on Windows): assume colours work.
        None => true,
    }
}

/// The resolved style of every slot. `Copy` so it can be handed around freely.
///
/// A couple of slots (BOX_TITLE, KEY) are only used by the centered windows of
/// the C version, which ncdurs does not draw yet; they are kept so the table
/// stays a complete mirror of UI_COLORS.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct Colors {
    /// UIC_DEFAULT
    pub default: Style,
    /// UIC_BOX_TITLE - titles of the centered windows
    pub box_title: Style,
    /// UIC_HD - header (status line) and footer (totals)
    pub hd: Style,
    /// UIC_SEL - the selected row
    pub sel: Style,
    /// UIC_NUM - the number part of a size in the listing
    pub num: Style,
    /// UIC_NUM_HD - numbers in the footer
    pub num_hd: Style,
    /// UIC_NUM_SEL - numbers in the selected row
    pub num_sel: Style,
    /// UIC_KEY - a key hint ("i" in the info window, ...)
    pub key: Style,
    /// UIC_KEY_HD - a highlighted key hint ("?" in the status line)
    pub key_hd: Style,
    /// UIC_DIR - a directory name
    pub dir: Style,
    /// UIC_DIR_SEL - a directory name in the selected row
    pub dir_sel: Style,
    /// UIC_FLAG - the one character flag column
    pub flag: Style,
    /// UIC_FLAG_SEL
    pub flag_sel: Style,
    /// UIC_GRAPH - the bar in the graph column
    pub graph: Style,
    /// UIC_GRAPH_SEL
    pub graph_sel: Style,
}

impl Default for Colors {
    fn default() -> Self {
        Theme::default().colors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_schemes() {
        assert_eq!(Theme::parse("auto"), Some(Theme::Auto));
        assert_eq!(Theme::parse("off"), Some(Theme::Off));
        assert_eq!(Theme::parse("dark"), Some(Theme::Dark));
        assert_eq!(Theme::parse("dark-bg"), Some(Theme::DarkBg));
        assert_eq!(Theme::parse("DARK"), None);
        assert_eq!(Theme::parse(""), None);
        assert_eq!(Theme::default(), Theme::Auto);
    }

    #[test]
    fn auto_needs_a_colour_capable_terminal() {
        assert!(supports_colors(false, Some("xterm-256color")));
        assert!(!supports_colors(false, Some("dumb")));
        assert!(!supports_colors(false, Some("")));
        // NO_COLOR wins over an otherwise capable terminal.
        assert!(!supports_colors(true, Some("xterm-256color")));
        // No TERM at all (Windows): assume colours work.
        assert!(supports_colors(false, None));
    }

    /// A few slots from src/util.h, checked literally.
    #[test]
    fn matches_the_c_table() {
        let off = Theme::Off.colors();
        assert_eq!(off.dir, Style::default());
        assert_eq!(off.flag, Style::default());
        assert!(off.key_hd.add_modifier.contains(Modifier::BOLD));
        assert!(off.key_hd.add_modifier.contains(Modifier::REVERSED));

        let dark = Theme::Dark.colors();
        assert_eq!(dark.hd, Style::default().fg(Color::Black).bg(Color::Cyan));
        assert_eq!(
            dark.dir,
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(dark.flag, Style::default().fg(Color::Red));
        // COLOR_WHITE is ANSI 7, which ratatui calls Gray (White would be 15).
        assert_eq!(dark.sel.fg, Some(Color::Gray));
        assert_eq!(dark.sel.bg, Some(Color::Green));

        // dark-bg differs from dark only where a background was explicit.
        let darkbg = Theme::DarkBg.colors();
        assert_eq!(darkbg.num.fg, dark.num.fg);
        assert_eq!(darkbg.num.bg, Some(Color::Black));
        assert_eq!(darkbg.default.bg, Some(Color::Black));
        assert_eq!(darkbg.hd, dark.hd);
    }
}
