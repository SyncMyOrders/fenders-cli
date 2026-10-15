//! `fenders logs` — app output, decoded from base64, with `-f` to follow.
//!
//! The logs endpoint pages forward with an `after` polling cursor and reports
//! `has_more` while stored records remain.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use clap::Args;
use serde_json::Value;

use crate::app::App;
use crate::ui::{self, To};

#[derive(Args, Debug)]
pub struct LogsArgs {
    pub app: String,
    /// Keep polling for new output
    #[arg(short, long)]
    pub follow: bool,
    /// Only this run
    #[arg(long)]
    pub run: Option<String>,
    /// Only this release
    #[arg(long)]
    pub release: Option<String>,
    /// Only this execution
    #[arg(long)]
    pub execution: Option<String>,
    /// Maximum records to print (without --follow)
    #[arg(short = 'n', long, default_value_t = 500)]
    pub limit: usize,
    /// Seconds between polls when following
    #[arg(long, default_value_t = 2, value_name = "SECS")]
    pub interval: u64,
}

const TEXT_KEYS: &[&str] = &[
    "output", "data", "content", "chunk", "message", "line", "text",
];
const TIME_KEYS: &[&str] = &["timestamp", "time", "ts", "created", "created_at", "at"];

/// Decode base64 output; fall back to the raw string.
fn decode(s: &str) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    match engine.decode(s.trim()) {
        Ok(bytes) if !s.trim().is_empty() => match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        },
        _ => s.to_string(),
    }
}

/// The record's output text. Output-carrying fields are base64; message-like
/// fields are plain text.
pub fn text_of(rec: &Value) -> Option<String> {
    TEXT_KEYS.iter().find_map(|k| {
        let s = rec.get(*k)?.as_str()?;
        Some(if B64_KEYS.contains(k) {
            decode(s)
        } else {
            s.to_string()
        })
    })
}

const B64_KEYS: &[&str] = &["output", "data", "content", "chunk"];

fn time_of(rec: &Value) -> Option<String> {
    let raw = TIME_KEYS
        .iter()
        .find_map(|k| rec.get(*k).and_then(Value::as_str))?;
    let t = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    Some(
        t.with_timezone(&chrono::Local)
            .format("%H:%M:%S%.3f")
            .to_string(),
    )
}

fn render(rec: &Value, show_run: bool, json: bool) {
    if json {
        let mut r = rec.clone();
        if let (Some(t), Some(o)) = (text_of(rec), r.as_object_mut()) {
            o.insert("text".into(), Value::String(t));
        }
        println!("{}", serde_json::to_string(&r).unwrap_or_default());
        return;
    }
    let ts = time_of(rec)
        .map(|t| ui::dim(&t, To::Out))
        .unwrap_or_default();
    let stream = ["stream", "level", "fd"]
        .iter()
        .find_map(|k| rec.get(*k).and_then(Value::as_str));
    let tag = match stream {
        Some("stderr" | "error" | "err" | "warn" | "warning") => ui::rgb("err", ui::RED, To::Out),
        Some(_) => ui::dim("out", To::Out),
        None => String::new(),
    };
    let run = if show_run {
        rec.get("run_id")
            .and_then(Value::as_str)
            .map(|r| ui::rgb(&r.chars().take(8).collect::<String>(), ui::BAND, To::Out))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let prefix: Vec<String> = [ts, run, tag]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    let prefix = prefix.join(" ");
    let text = text_of(rec).unwrap_or_else(|| rec.to_string());
    for line in text.trim_end_matches('\n').split('\n') {
        // App output is untrusted: neutralise terminal escape sequences.
        let line = ui::sanitize(line.strip_suffix('\r').unwrap_or(line));
        if prefix.is_empty() {
            println!("{line}");
        } else {
            println!("{prefix}  {line}");
        }
    }
}

/// Remembers recently printed records so re-polled pages don't print twice.
struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    fn new() -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    fn first_time(&mut self, rec: &Value) -> bool {
        let key = rec
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| rec.to_string());
        if !self.set.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > 5000
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }
}

pub fn run(app: &mut App, args: LogsArgs) -> Result<()> {
    let p = app.app_path(&args.app)?;
    let mut q = vec![];
    for (k, v) in [
        ("run_id", &args.run),
        ("release_id", &args.release),
        ("execution_id", &args.execution),
    ] {
        if let Some(v) = v {
            q.push((k.to_string(), v.clone()));
        }
    }
    stream(
        app,
        &format!("{p}/logs"),
        q,
        args.follow,
        args.limit,
        args.run.is_none(),
        args.interval,
    )
}

pub fn print_for_run(app: &mut App, app_path: &str, run_id: &str) -> Result<()> {
    let q = vec![("run_id".to_string(), run_id.to_string())];
    stream(app, &format!("{app_path}/logs"), q, false, 10_000, false, 2)
}

fn stream(
    app: &mut App,
    path: &str,
    base: Vec<(String, String)>,
    follow: bool,
    limit: usize,
    show_run: bool,
    interval: u64,
) -> Result<()> {
    let json = app.out.json;
    let mut seen = Seen::new();
    let mut after: Option<String> = None;
    let mut printed = 0usize;
    if follow && !json {
        ui::info(&format!(
            "Following logs {}",
            ui::dim("(Ctrl-C to stop)", To::Err)
        ));
    }
    loop {
        let mut q = base.clone();
        q.push(("limit".into(), "100".into()));
        if let Some(a) = &after {
            q.push(("after".into(), a.clone()));
        }
        let page = app.client.get(path, &q)?;
        for rec in crate::app::items_of(&page) {
            if seen.first_time(&rec) {
                render(&rec, show_run, json);
                printed += 1;
                if !follow && printed >= limit {
                    return Ok(());
                }
            }
        }
        let next = match page.get("next") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let has_more = page
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if next.is_some() {
            after = next;
        }
        if has_more && after.is_some() {
            continue;
        }
        if !follow {
            if printed == 0 && !json {
                ui::info("No log output yet.");
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(interval.max(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_base64_output() {
        assert_eq!(
            text_of(&json!({"output": "aGVsbG8K"})).as_deref(),
            Some("hello\n")
        );
    }

    #[test]
    fn keeps_plain_text() {
        assert_eq!(
            text_of(&json!({"message": "not base64!"})).as_deref(),
            Some("not base64!")
        );
    }

    #[test]
    fn neutralises_terminal_escapes() {
        // Clear screen + OSC 52 clipboard write + BEL.
        let evil = "hi\x1b[2J\x1b]52;c;ZXZpbA==\x07";
        let safe = ui::sanitize(evil);
        assert!(!safe.chars().any(|c| c.is_control()));
        assert_eq!(safe, "hi\\u{1b}[2J\\u{1b}]52;c;ZXZpbA==\\u{7}");
        assert_eq!(ui::sanitize("tab\tok"), "tab\tok");
    }

    #[test]
    fn dedupes_records() {
        let mut s = Seen::new();
        assert!(s.first_time(&json!({"id": "a"})));
        assert!(!s.first_time(&json!({"id": "a"})));
        assert!(s.first_time(&json!({"id": "b"})));
    }
}
