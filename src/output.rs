//! Rendering of API records: tables and key/value views for humans, JSON for
//! machines. Records are untyped, so columns are chosen from the data.

use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table, presets};
use serde_json::Value;

use crate::ui::{self, To};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Tables on a terminal, JSON when piped
    Auto,
    Table,
    Json,
}

/// Fields shown first, in this order, when present.
const PREFERRED: &[&str] = &[
    "id",
    "name",
    "kind",
    "type",
    "state",
    "status",
    "desired",
    "phase",
    "size",
    "source",
    "revision",
    "version",
    "origin",
    "host",
    "port",
    "url",
    "created",
    "created_at",
    "updated",
    "updated_at",
];
const MAX_COLUMNS: usize = 7;
const MAX_CELL: usize = 56;

pub struct Out {
    pub json: bool,
}

impl Out {
    pub fn new(f: Format) -> Self {
        Self {
            json: match f {
                Format::Json => true,
                Format::Table => false,
                Format::Auto => !ui::stdout_is_tty(),
            },
        }
    }

    pub fn print_json(&self, v: &Value) {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    }

    /// Print a list. `cols` are preferred columns for this resource; others
    /// are added from the data.
    pub fn list(&self, items: &[Value], cols: &[&str], noun: &str) {
        if self.json {
            return self.print_json(&Value::Array(items.to_vec()));
        }
        if items.is_empty() {
            ui::info(&format!("No {noun} found."));
            return;
        }
        if !items.iter().all(Value::is_object) {
            for i in items {
                println!("{}", scalar(i));
            }
            return;
        }
        let columns = pick_columns(items, cols);
        let color = ui::color_out();
        let mut t = Table::new();
        t.load_style(presets::NOTHING);
        if !color {
            t.force_no_tty();
        }
        // Wrap to the terminal only when its width is actually known; a pty
        // reporting 0 columns would otherwise squeeze every column to 1 char.
        let width = t.width().filter(|w| *w >= 40).or_else(|| {
            std::env::var("COLUMNS")
                .ok()?
                .parse()
                .ok()
                .filter(|w| *w >= 40)
        });
        match width {
            Some(w) => {
                t.set_width(w)
                    .set_content_arrangement(ContentArrangement::Dynamic);
            }
            None => {
                t.set_content_arrangement(ContentArrangement::Disabled);
            }
        }
        t.set_header(columns.iter().map(|c| {
            let cell = Cell::new(ui::sanitize(&c.replace('_', " ").to_uppercase()));
            if color {
                cell.fg(Color::Rgb {
                    r: ui::ORANGE.0,
                    g: ui::ORANGE.1,
                    b: ui::ORANGE.2,
                })
                .add_attribute(Attribute::Bold)
            } else {
                cell
            }
        }));
        for item in items {
            t.add_row(
                columns
                    .iter()
                    .map(|c| cell(c, item.get(c.as_str()).unwrap_or(&Value::Null), color)),
            );
        }
        for col in t.column_iter_mut() {
            col.set_padding((0, 2));
        }
        println!("{t}");
    }

    /// Print a single record as aligned key/value pairs.
    pub fn record(&self, v: &Value) {
        if self.json {
            return self.print_json(v);
        }
        match v {
            Value::Object(_) => print_kv(v, 0),
            Value::Array(a) => self.list(a, &[], "items"),
            Value::Null => {}
            other => println!("{}", scalar(other)),
        }
    }

    /// Print whatever came back: pages and arrays as tables, objects as records.
    pub fn auto(&self, v: &Value, cols: &[&str], noun: &str) {
        match v {
            Value::Array(a) => self.list(a, cols, noun),
            Value::Object(o)
                if o.contains_key("items")
                    && o.get("items").is_some_and(Value::is_array)
                    && !self.json =>
            {
                self.list(o["items"].as_array().unwrap(), cols, noun)
            }
            _ => self.record(v),
        }
    }
}

pub fn is_scalar(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.iter().all(|x| !x.is_object() && !x.is_array()) && a.len() <= 6,
        Value::Object(_) => false,
        _ => true,
    }
}

fn pick_columns(items: &[Value], preferred: &[&str]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        if let Some(o) = item.as_object() {
            for (k, v) in o {
                if !seen.contains(k) && (is_scalar(v) || v.is_null()) {
                    seen.push(k.clone());
                }
            }
        }
    }
    // Drop columns that are null everywhere.
    seen.retain(|k| items.iter().any(|i| i.get(k).is_some_and(|v| !v.is_null())));
    let rank = |k: &str| {
        preferred
            .iter()
            .position(|p| *p == k)
            .map(|p| p as i32 - 1000)
            .or_else(|| PREFERRED.iter().position(|p| *p == k).map(|p| p as i32))
            .unwrap_or(500)
    };
    seen.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.cmp(b)));
    seen.truncate(MAX_COLUMNS);
    seen
}

fn is_time_key(k: &str) -> bool {
    k == "created"
        || k == "updated"
        || k.ends_with("_at")
        || k.ends_with("time")
        || k == "expires"
        || k == "timestamp"
}

fn is_state_key(k: &str) -> bool {
    matches!(
        k,
        "state" | "status" | "desired" | "phase" | "health" | "readiness" | "observed"
    )
}

pub fn scalar(v: &Value) -> String {
    match v {
        Value::Null => "—".into(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
        Value::String(s) => ui::sanitize(s).into_owned(),
        Value::Array(a) => a.iter().map(scalar).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

fn state_color(s: &str) -> Option<Color> {
    ui::state_rgb(s).map(|c| Color::Rgb {
        r: c.0,
        g: c.1,
        b: c.2,
    })
}

fn cell(key: &str, v: &Value, color: bool) -> Cell {
    let muted = Color::Rgb {
        r: ui::MUTED.0,
        g: ui::MUTED.1,
        b: ui::MUTED.2,
    };
    if v.is_null() {
        let c = Cell::new("—");
        return if color { c.fg(muted) } else { c };
    }
    let text = scalar(v);
    if is_time_key(key)
        && let Some(rel) = ui::relative_time(&text)
    {
        let c = Cell::new(rel);
        return if color { c.fg(muted) } else { c };
    }
    let c = Cell::new(truncate(&text, MAX_CELL));
    if color
        && is_state_key(key)
        && let Some(col) = state_color(&text)
    {
        return c.fg(col);
    }
    c
}

fn print_kv(v: &Value, indent: usize) {
    let Some(o) = v.as_object() else {
        println!("{}{}", " ".repeat(indent), scalar(v));
        return;
    };
    let nested = |v: &Value| match v {
        Value::Object(m) => !m.is_empty(),
        Value::Array(a) => a.iter().any(|x| x.is_object() || x.is_array()),
        _ => false,
    };
    let width = o
        .iter()
        .filter(|(_, v)| !nested(v))
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let pad = " ".repeat(indent);
    // Scalars first, then nested blocks, each in API order.
    let ordered = o
        .iter()
        .filter(|(_, v)| !nested(v))
        .chain(o.iter().filter(|(_, v)| nested(v)));
    for (k, val) in ordered {
        let k: &str = &ui::sanitize(k);
        let key = ui::dim(&format!("{k:width$}"), To::Out);
        match val {
            Value::Object(m) if m.is_empty() => println!("{pad}{key}  {}", ui::dim("{}", To::Out)),
            Value::Object(_) => {
                println!("{pad}{}", ui::dim(k, To::Out));
                print_kv(val, indent + 2);
            }
            Value::Array(a) if a.iter().any(|x| x.is_object() || x.is_array()) => {
                println!("{pad}{}", ui::dim(k, To::Out));
                for item in a {
                    println!("{pad}  {}", ui::accent("•", To::Out));
                    print_kv(item, indent + 4);
                }
            }
            Value::Array(a) if a.is_empty() => println!("{pad}{key}  {}", ui::dim("[]", To::Out)),
            Value::Null => println!("{pad}{key}  {}", ui::dim("—", To::Out)),
            _ => {
                let s = scalar(val);
                let shown = if is_state_key(k) {
                    ui::state(&s, To::Out)
                } else if is_time_key(k) {
                    match ui::relative_time(&s) {
                        Some(rel) => format!("{s} {}", ui::dim(&format!("({rel})"), To::Out)),
                        None => s,
                    }
                } else {
                    s
                };
                println!("{pad}{key}  {shown}");
            }
        }
    }
}
