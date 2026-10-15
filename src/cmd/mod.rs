pub mod api;
pub mod apps;
pub mod deploy;
pub mod logs;
pub mod project;
pub mod session;
pub mod update;

use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, ValueEnum};
use serde_json::{Map, Value, json};

use crate::app::{App, id_of};
use crate::client::{Body, MAX_WASM_BYTES};
use crate::output::Out;
use crate::ui::{self, To};

#[derive(Args, Debug, Clone)]
pub struct ListArgs {
    /// Maximum number of items to show
    #[arg(short = 'n', long, value_name = "N")]
    pub limit: Option<usize>,
    /// Fetch every page
    #[arg(long, conflicts_with = "limit")]
    pub all: bool,
}

impl ListArgs {
    /// How many items to fetch: everything when piped/JSON, a screenful otherwise.
    pub fn want(&self, out: &Out) -> Option<usize> {
        if self.all {
            None
        } else if let Some(n) = self.limit {
            Some(n.max(1))
        } else if out.json {
            None
        } else {
            Some(25)
        }
    }

    pub fn more_hint(&self, more: bool, shown: usize) {
        if more {
            ui::hint(&format!(
                "Showing {shown}. Use --all or --limit N to see more."
            ));
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct WaitArgs {
    /// Return immediately with the operation instead of waiting for it
    #[arg(long)]
    pub no_wait: bool,
    /// Seconds to wait for the operation to finish
    #[arg(long, default_value_t = 600, value_name = "SECS")]
    pub timeout: u64,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum Size {
    Small,
    Medium,
    Large,
}

/// App runtime configuration (`Configuration` in the API).
#[derive(Args, Debug, Clone, Default)]
pub struct ConfigArgs {
    /// Instance size (see `fenders sizes`)
    #[arg(long, value_enum)]
    pub size: Option<Size>,
    /// Environment variable, repeatable
    #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Expose a project secret as an environment variable, repeatable
    #[arg(long = "secret-env", value_name = "KEY=SECRET")]
    pub secret_env: Vec<String>,
    /// Keep an instance warm to avoid cold starts
    #[arg(long, overrides_with = "no_keep_warm")]
    pub keep_warm: bool,
    /// Let the app scale to zero
    #[arg(long)]
    pub no_keep_warm: bool,
}

fn split_kv(s: &str, what: &str) -> Result<(String, String)> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("{what} must look like KEY=VALUE, got \"{s}\""))?;
    if k.is_empty() {
        bail!("{what} has an empty key: \"{s}\"");
    }
    Ok((k.to_string(), v.to_string()))
}

impl ConfigArgs {
    /// The `Configuration` object, or `None` when no flag was given.
    pub fn to_json(&self, app: &mut App) -> Result<Option<Value>> {
        let mut c = Map::new();
        if let Some(s) = self.size {
            c.insert("size".into(), json!(format!("{s:?}").to_lowercase()));
        }
        if !self.env.is_empty() {
            let mut m = Map::new();
            for e in &self.env {
                let (k, v) = split_kv(e, "--env")?;
                m.insert(k, json!(v));
            }
            c.insert("env".into(), Value::Object(m));
        }
        if !self.secret_env.is_empty() {
            let mut m = Map::new();
            for e in &self.secret_env {
                let (k, v) = split_kv(e, "--secret-env")?;
                m.insert(k, json!(app.resolve_in("secrets", &v, "secret")?));
            }
            c.insert("secret_env".into(), Value::Object(m));
        }
        if self.keep_warm {
            c.insert("keep_warm".into(), json!(true));
        } else if self.no_keep_warm {
            c.insert("keep_warm".into(), json!(false));
        }
        Ok((!c.is_empty()).then_some(Value::Object(c)))
    }
}

/// Ask before something destructive. Requires `--yes` when not interactive.
pub fn confirm(prompt: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !ui::interactive() {
        bail!("refusing to continue without confirmation — pass --yes");
    }
    let ok = dialoguer::Confirm::with_theme(&ui::theme())
        .with_prompt(prompt)
        .default(false)
        .interact()?;
    if ok { Ok(()) } else { bail!("aborted") }
}

/// Upload a WASI component and return the artifact record.
pub fn upload_wasm(app: &mut App, file: &Path) -> Result<Value> {
    let meta =
        std::fs::metadata(file).with_context(|| format!("cannot read {}", file.display()))?;
    if meta.len() > MAX_WASM_BYTES {
        bail!(
            "{} is {:.1} MiB; WASI components may be at most 16 MiB",
            file.display(),
            meta.len() as f64 / 1048576.0
        );
    }
    let bytes = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    if !bytes.starts_with(b"\0asm") {
        bail!(
            "{} is not a WebAssembly binary (missing \\0asm magic)",
            file.display()
        );
    }
    let path = format!("{}/artifacts", app.proj_path()?);
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let pb = ui::spinner(format!("Uploading {name} ({})", human_bytes(meta.len())));
    let res = app
        .client
        .send(reqwest::Method::POST, &path, &[], Body::Wasm(bytes));
    pb.finish_and_clear();
    let rec = res?;
    let id = id_of(&rec).ok_or_else(|| anyhow!("upload response had no artifact id: {rec}"))?;
    ui::success(&format!(
        "Uploaded {name} {}",
        ui::dim(&format!("artifact {id}"), To::Err)
    ));
    Ok(rec)
}

/// Accept an artifact id/name or a path to a `.wasm` file (which is uploaded).
pub fn artifact_ref(app: &mut App, s: &str) -> Result<String> {
    let p = Path::new(s);
    if p.is_file() {
        let rec = upload_wasm(app, p)?;
        return id_of(&rec).context("artifact has no id");
    }
    app.resolve_in("artifacts", s, "artifact")
}

/// Look for problems in an artifact inspection record. Returns an error
/// message when the component is clearly not deployable. Server-provided text
/// is sanitized here because callers print it directly (e.g. via `ui::warn`).
pub fn check_inspection(insp: &Value) -> Option<String> {
    let bad_state = ["state", "status", "validation", "result"]
        .iter()
        .find_map(|k| {
            let s = insp.get(*k)?.as_str()?;
            matches!(
                s.to_ascii_lowercase().as_str(),
                "invalid" | "failed" | "rejected" | "error"
            )
            .then(|| s.to_string())
        });
    if let Some(e) = insp.get("error").and_then(Value::as_str) {
        return Some(format!(
            "the component did not pass validation: {}",
            ui::sanitize(e)
        ));
    }
    let invalid_flag = insp.get("valid").and_then(Value::as_bool) == Some(false);
    let errors: Vec<String> = ["errors", "problems"]
        .iter()
        .filter_map(|k| insp.get(*k)?.as_array())
        .flatten()
        .map(|e| {
            ui::sanitize(
                &e.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| e.to_string()),
            )
            .into_owned()
        })
        .collect();
    if bad_state.is_some() || invalid_flag || !errors.is_empty() {
        let mut msg = "the component did not pass validation".to_string();
        if !errors.is_empty() {
            msg.push_str(": ");
            msg.push_str(&errors.join("; "));
        }
        return Some(msg);
    }
    None
}

pub fn human_bytes(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KiB", n as f64 / 1024.0),
        _ => format!("{:.1} MiB", n as f64 / 1_048_576.0),
    }
}

/// Read a value from stdin when piped, or from a hidden prompt.
pub fn read_secret(prompt: &str, confirm_twice: bool) -> Result<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut s = String::new();
        stdin.lock().read_to_string(&mut s)?;
        return Ok(s.trim_end_matches(['\r', '\n']).to_string());
    }
    if !ui::interactive() {
        bail!("no value on stdin and prompts are disabled");
    }
    let theme = ui::theme();
    let mut p = dialoguer::Password::with_theme(&theme).with_prompt(prompt);
    if confirm_twice {
        p = p.with_confirmation("Confirm", "Values did not match");
    }
    Ok(p.interact()?)
}

#[cfg(test)]
mod tests {
    use super::check_inspection;
    use serde_json::json;

    #[test]
    fn inspection_problems_cannot_inject_terminal_escapes() {
        let evil = "bad\x1b]52;c;ZXZpbA==\x07\x1b[2J";
        for insp in [
            json!({"error": evil}),
            json!({"valid": false, "errors": [evil]}),
        ] {
            let msg = check_inspection(&insp).expect("a problem");
            assert!(!msg.chars().any(|c| c.is_control()), "{msg:?}");
        }
        assert_eq!(
            check_inspection(&json!({"error": null, "inspection": {"http": true}})),
            None
        );
    }
}
