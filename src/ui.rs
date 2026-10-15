//! Terminal styling, the fender banner, spinners and status messages.
//!
//! Data goes to stdout; everything conversational (progress, hints, errors)
//! goes to stderr so `fenders … --json | jq` stays clean.

use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

/// Brand palette, taken from fenders.io.
pub const ORANGE: (u8, u8, u8) = (255, 96, 20); // #FF6014
pub const EMBER: (u8, u8, u8) = (223, 62, 4); // #DF3E04
pub const MUTED: (u8, u8, u8) = (169, 182, 192); // #A9B6C0
pub const GREEN: (u8, u8, u8) = (111, 211, 165); // #6FD3A5
pub const RED: (u8, u8, u8) = (240, 82, 64);
pub const YELLOW: (u8, u8, u8) = (245, 190, 70);
pub const BAND: (u8, u8, u8) = (79, 111, 140); // #4F6F8C ("sounding")

static COLOR_OUT: OnceLock<bool> = OnceLock::new();
static COLOR_ERR: OnceLock<bool> = OnceLock::new();
static INTERACTIVE: OnceLock<bool> = OnceLock::new();

pub fn init(no_color: bool, no_input: bool) {
    let allowed = !no_color
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true);
    let _ = COLOR_OUT.set(allowed && std::io::stdout().is_terminal());
    let _ = COLOR_ERR.set(allowed && std::io::stderr().is_terminal());
    let _ = INTERACTIVE
        .set(!no_input && std::io::stdin().is_terminal() && std::io::stderr().is_terminal());
}

pub fn color_out() -> bool {
    *COLOR_OUT.get().unwrap_or(&false)
}

pub fn color_err() -> bool {
    *COLOR_ERR.get().unwrap_or(&false)
}

/// True when we may prompt the user.
pub fn interactive() -> bool {
    *INTERACTIVE.get().unwrap_or(&false)
}

pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Which stream a styled string is destined for.
#[derive(Clone, Copy)]
pub enum To {
    Out,
    Err,
}

fn on(to: To) -> bool {
    match to {
        To::Out => color_out(),
        To::Err => color_err(),
    }
}

pub fn rgb(s: &str, c: (u8, u8, u8), to: To) -> String {
    if on(to) {
        s.truecolor(c.0, c.1, c.2).to_string()
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str, to: To) -> String {
    if on(to) {
        s.bold().to_string()
    } else {
        s.to_string()
    }
}

pub fn accent(s: &str, to: To) -> String {
    if on(to) {
        s.truecolor(ORANGE.0, ORANGE.1, ORANGE.2).bold().to_string()
    } else {
        s.to_string()
    }
}

pub fn dim(s: &str, to: To) -> String {
    rgb(s, MUTED, to)
}

/// Colour a status/state word by meaning.
pub fn state(s: &str, to: To) -> String {
    match state_rgb(s) {
        Some(c) => rgb(s, c, to),
        None => s.to_string(),
    }
}

pub fn state_rgb(s: &str) -> Option<(u8, u8, u8)> {
    Some(match s.to_ascii_lowercase().as_str() {
        "succeeded" | "success" | "enabled" | "running" | "ready" | "active" | "healthy" | "ok"
        | "valid" | "allowed" | "live" | "deployed" | "completed" | "published" => GREEN,
        "pending" | "queued" | "starting" | "deploying" | "stopping" | "provisioning"
        | "building" | "draft" | "warn" | "warning" | "in_progress" => YELLOW,
        "failed" | "error" | "errored" | "denied" | "invalid" | "revoked" | "crashed"
        | "rejected" | "expired" => RED,
        "stopped" | "disabled" | "inactive" | "cancelled" | "canceled" => MUTED,
        _ => return None,
    })
}

/// The fenders.io mark: an orange maritime fender with two dark bands,
/// hanging from its rope, next to the wordmark.
pub fn banner() -> String {
    let o = |s: &str| rgb(s, ORANGE, To::Err);
    let e = |s: &str| rgb(s, EMBER, To::Err);
    let b = |s: &str| rgb(s, BAND, To::Err);
    let word = format!("{}{}", bold("fenders", To::Err), accent(".io", To::Err));
    let tag = dim("WASM sandboxes in hardware-isolated microVMs", To::Err);
    let lines = [
        format!("    {}", b("▐▌")),
        format!("  {}{}", o("▄███"), e("█▄")),
        format!("  {}{}    {}", o("████"), e("██"), word),
        format!("  {}    {}", b("▀▀▀▀▀▀"), tag),
        format!("  {}{}", o("████"), e("██")),
        format!("  {}", b("▀▀▀▀▀▀")),
        format!("  {}{}", o("▀███"), e("█▀")),
    ];
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

pub fn success(msg: &str) {
    eprintln!("{} {}", rgb("✓", GREEN, To::Err), msg);
}

pub fn info(msg: &str) {
    eprintln!("{} {}", accent("›", To::Err), msg);
}

pub fn warn(msg: &str) {
    eprintln!("{} {}", rgb("!", YELLOW, To::Err), msg);
}

pub fn hint(msg: &str) {
    eprintln!("  {}", dim(msg, To::Err));
}

pub fn error(msg: &str) {
    eprintln!("{} {}", rgb("✗", RED, To::Err), msg);
}

/// A branded spinner on stderr. Hidden automatically when stderr is not a TTY.
pub fn spinner(msg: impl Into<String>) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    let tmpl = if color_err() {
        "{spinner:.208} {msg} {elapsed:.dim}"
    } else {
        "{spinner} {msg} {elapsed}"
    };
    pb.set_style(
        ProgressStyle::with_template(tmpl)
            .unwrap()
            .tick_strings(&["◜", "◠", "◝", "◞", "◡", "◟", "●"]),
    );
    pb.set_message(msg.into());
    pb.enable_steady_tick(Duration::from_millis(90));
    pb
}

pub fn theme() -> dialoguer::theme::ColorfulTheme {
    use dialoguer::console::Style;
    let mut t = dialoguer::theme::ColorfulTheme::default();
    if color_err() {
        let orange = Style::new().color256(208);
        t.prompt_prefix = orange.apply_to("?".to_string());
        t.active_item_prefix = orange.apply_to("❯".to_string());
        t.active_item_style = Style::new().color256(208).bold();
        t.success_prefix = Style::new().green().apply_to("✓".to_string());
        t.values_style = Style::new().color256(208);
    }
    t
}

/// "3 min ago" style rendering for RFC 3339 timestamps.
pub fn relative_time(s: &str) -> Option<String> {
    let t = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds();
    let (future, secs) = if secs < 0 {
        (true, -secs)
    } else {
        (false, secs)
    };
    let span = match secs {
        0..=44 => "just now".to_string(),
        45..=89 => "1 min".to_string(),
        90..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        86_400..=2_591_999 => format!("{} d", secs / 86_400),
        _ => return Some(t.format("%Y-%m-%d").to_string()),
    };
    if span == "just now" {
        Some(span)
    } else if future {
        Some(format!("in {span}"))
    } else {
        Some(format!("{span} ago"))
    }
}
