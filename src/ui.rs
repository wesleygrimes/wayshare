//! Terminal output for the interactive commands. Labels and values match the success lines
//! demand prints after a prompt, so prompted and automatic steps read the same.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::OnceLock;

use console::{Style, style};
use demand::Theme;
use termcolor::Color;

const TERRACOTTA: u8 = 173;
const SAGE: u8 = 107;
const BRICK: u8 = 167;
const GOLD: u8 = 179;
const TAN: u8 = 180;
const GRAY: u8 = 245;

fn label() -> Style {
    Style::new().color256(TERRACOTTA).bold()
}

/// The demand theme, in the same colors as the rest of the output.
pub fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        if !console::colors_enabled_stderr() {
            return Theme::new();
        }
        let fg = |n| Theme::color_ansi256(n);
        let mut title = fg(TERRACOTTA);
        title.set_bold(true);
        let mut focused_button = fg(232);
        focused_button.set_bg(Some(Color::Ansi256(TERRACOTTA)));
        let mut blurred_button = fg(250);
        blurred_button.set_bg(Some(Color::Ansi256(238)));
        let mut t = Theme::charm();
        t.title = title;
        t.description = fg(GRAY);
        t.cursor = fg(TERRACOTTA);
        t.selected_prefix_fg = fg(SAGE);
        t.selected_option = fg(SAGE);
        t.unselected_prefix_fg = fg(GRAY);
        t.input_cursor = fg(SAGE);
        t.input_prompt = fg(TERRACOTTA);
        t.error_indicator = fg(BRICK);
        t.focused_button = focused_button;
        t.blurred_button = blurred_button;
        t.breadcrumb_active = fg(TERRACOTTA);
        t.breadcrumb_clickable = fg(TAN);
        t.help_key = fg(GRAY);
        t
    })
}

pub fn title(text: &str) {
    println!(
        "\n{}\n",
        style(text).color256(TERRACOTTA).bold().underlined()
    );
}

pub fn done(what: &str, value: &str) {
    println!("{} {}", label().apply_to(what), style(value).color256(SAGE));
}

pub fn warn(what: &str, value: &str) {
    println!("{} {}", label().apply_to(what), style(value).color256(GOLD));
}

pub fn fail(what: &str, value: &str) {
    println!(
        "{} {}",
        label().apply_to(what),
        style(value).color256(BRICK)
    );
}

pub fn hint(text: &str) {
    println!(
        "  {} {}",
        style("→").color256(GRAY),
        style(text).color256(GRAY)
    );
}

pub fn check(passed: Option<bool>, what: &str, detail: &str) {
    let (mark, color) = match passed {
        Some(true) => ("✓", SAGE),
        Some(false) => ("✗", BRICK),
        None => ("!", GOLD),
    };
    println!(
        "{} {:<17} {}",
        style(mark).color256(color).bold(),
        label().apply_to(what),
        style(detail).color256(color)
    );
}

pub fn check_hint(text: &str) {
    println!("  {:<17} {}", "", style(format!("→ {text}")).color256(GRAY));
}

pub fn heading(text: &str) {
    println!("\n{}", style(text).color256(SAGE).bold());
}

pub fn alert(text: &str) {
    println!("\n{}", style(text).color256(BRICK).bold());
}

pub fn line(text: &str) {
    println!("  {text}");
}

pub fn dim(text: &str) -> String {
    style(text).color256(GRAY).to_string()
}

pub fn code(text: &str) -> String {
    style(text).color256(TAN).to_string()
}

/// `path` with the home directory shown as `~`.
pub fn path(path: &Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap().display())
        }
        _ => path.display().to_string(),
    }
}

/// Runs `f` behind a spinner when attached to a terminal.
pub fn spin<T: Send>(title: &str, f: impl FnOnce() -> T + Send) -> T {
    if !std::io::stderr().is_terminal() {
        return f();
    }
    let mut task = Some(f);
    let mut result = None;
    // If the spinner can't draw, it may return before running the task; run it plainly then.
    let _ = demand::Spinner::new(title)
        .theme(theme())
        .run(|_| result = task.take().map(|f| f()));
    match (result, task) {
        (Some(v), _) => v,
        (None, Some(f)) => f(),
        (None, None) => unreachable!("the task ran but returned nothing"),
    }
}

/// Prints an interactive command's error and exits: 130 when the user cancelled a prompt.
pub fn exit_with(err: anyhow::Error) -> ! {
    let cancelled = err
        .chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(|e| e.kind() == std::io::ErrorKind::Interrupted);
    if cancelled {
        println!("\n{}", dim("Cancelled."));
        std::process::exit(130);
    }
    eprintln!("\n{} {err:#}", style("✗").color256(BRICK).bold());
    std::process::exit(1);
}
