//! Terminal output for the interactive commands. Labels and values match the success lines
//! demand prints after a prompt, so prompted and automatic steps read the same.

use std::io::IsTerminal;
use std::path::Path;

use console::{Style, style};

const INDIGO: u8 = 99;
const GREEN: u8 = 36;
const RED: u8 = 204;
const YELLOW: u8 = 214;
const GRAY: u8 = 243;

fn label() -> Style {
    Style::new().color256(INDIGO).bold()
}

pub fn title(text: &str) {
    println!("\n{}\n", style(text).color256(INDIGO).bold().underlined());
}

pub fn done(what: &str, value: &str) {
    println!(
        "{} {}",
        label().apply_to(what),
        style(value).color256(GREEN)
    );
}

pub fn warn(what: &str, value: &str) {
    println!(
        "{} {}",
        label().apply_to(what),
        style(value).color256(YELLOW)
    );
}

pub fn fail(what: &str, value: &str) {
    println!("{} {}", label().apply_to(what), style(value).color256(RED));
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
        Some(true) => ("✓", GREEN),
        Some(false) => ("✗", RED),
        None => ("!", YELLOW),
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
    println!("\n{}", style(text).color256(GREEN).bold());
}

pub fn line(text: &str) {
    println!("  {text}");
}

pub fn dim(text: &str) -> String {
    style(text).color256(GRAY).to_string()
}

pub fn code(text: &str) -> String {
    style(text).color256(212).to_string()
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
    let _ = demand::Spinner::new(title).run(|_| result = task.take().map(|f| f()));
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
    eprintln!("\n{} {err:#}", style("✗").color256(RED).bold());
    std::process::exit(1);
}
