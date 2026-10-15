//! Shared command context: the API client, output settings, and resolution of
//! the current workspace/project, names → ids, pagination and operations.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::client::{Client, seg};
use crate::config::Config;
use crate::output::Out;
use crate::ui::{self, To};

pub fn is_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn id_of(v: &Value) -> Option<String> {
    v.get("id").and_then(Value::as_str).map(str::to_string)
}

pub fn name_of(v: &Value) -> Option<String> {
    ["name", "slug", "title", "display_name"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}

fn label(v: &Value) -> String {
    match (name_of(v), id_of(v)) {
        (Some(n), Some(i)) => format!("{n}  {}", ui::dim(&i, To::Err)),
        (Some(n), None) => n,
        (None, Some(i)) => i,
        _ => v.to_string(),
    }
}

/// Find `needle` among `items` by id, then exact name, then case-insensitive name.
pub fn find<'a>(items: &'a [Value], needle: &str) -> Option<&'a Value> {
    items
        .iter()
        .find(|v| id_of(v).as_deref() == Some(needle))
        .or_else(|| items.iter().find(|v| name_of(v).as_deref() == Some(needle)))
        .or_else(|| {
            items
                .iter()
                .find(|v| name_of(v).is_some_and(|n| n.eq_ignore_ascii_case(needle)))
        })
}

/// Page items from either a `Page` (`{items, next}`) or a bare array.
pub fn items_of(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => vec![],
    }
}

fn cursor_of(v: &Value) -> Option<String> {
    match v.get("next")? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The optimistic-concurrency revision of a record, wherever it lives.
pub fn revision_of(v: &Value) -> Option<i64> {
    const KEYS: &[&str] = &["revision", "expected_revision", "rev", "resource_revision"];
    let direct = |o: &Value| KEYS.iter().find_map(|k| o.get(*k).and_then(Value::as_i64));
    direct(v).or_else(|| {
        v.as_object()?
            .values()
            .filter(|x| x.is_object())
            .find_map(direct)
    })
}

pub struct App {
    pub client: Client,
    pub out: Out,
    pub cfg: Config,
    pub api_url: String,
    ws_flag: Option<String>,
    proj_flag: Option<String>,
    ws: Option<String>,
    proj: Option<String>,
}

impl App {
    pub fn new(
        client: Client,
        out: Out,
        cfg: Config,
        api_url: String,
        ws: Option<String>,
        proj: Option<String>,
    ) -> Self {
        Self {
            client,
            out,
            cfg,
            api_url,
            ws_flag: ws,
            proj_flag: proj,
            ws: None,
            proj: None,
        }
    }

    /// The current workspace id: flag/env → config → interactive picker.
    pub fn workspace(&mut self) -> Result<String> {
        if let Some(w) = &self.ws {
            return Ok(w.clone());
        }
        let id = if let Some(f) = self.ws_flag.clone() {
            self.resolve_workspace(&f)?.0
        } else if let Some(w) = self.cfg.workspace.clone() {
            w
        } else if ui::interactive() {
            let (id, name) = self.pick_workspace()?;
            self.cfg.set_workspace(id.clone(), name);
            self.cfg.save()?;
            ui::hint("Saved as your default workspace. Change it with `fenders use`.");
            id
        } else {
            bail!(missing("workspace", "--workspace", "FENDERS_WORKSPACE"));
        };
        self.ws = Some(id.clone());
        Ok(id)
    }

    /// The current project id: flag/env → config → interactive picker.
    pub fn project(&mut self) -> Result<String> {
        if let Some(p) = &self.proj {
            return Ok(p.clone());
        }
        let ws = self.workspace()?;
        let flag_ws_differs =
            self.ws_flag.is_some() && self.cfg.workspace.as_deref() != Some(ws.as_str());
        let id = if let Some(f) = self.proj_flag.clone() {
            self.resolve_project(&ws, &f)?.0
        } else if let (Some(p), false) = (self.cfg.project.clone(), flag_ws_differs) {
            p
        } else if ui::interactive() {
            let (id, name) = self.pick_project(&ws)?;
            if !flag_ws_differs {
                self.cfg.set_project(id.clone(), name);
                self.cfg.save()?;
                ui::hint("Saved as your default project. Change it with `fenders use`.");
            }
            id
        } else {
            bail!(missing("project", "--project", "FENDERS_PROJECT"));
        };
        self.proj = Some(id.clone());
        Ok(id)
    }

    pub fn ws_path(&mut self) -> Result<String> {
        Ok(format!("/workspaces/{}", seg(&self.workspace()?)))
    }

    pub fn proj_path(&mut self) -> Result<String> {
        let ws = self.ws_path()?;
        Ok(format!("{ws}/projects/{}", seg(&self.project()?)))
    }

    pub fn resolve_workspace(&self, needle: &str) -> Result<(String, Option<String>)> {
        if is_id(needle) {
            return Ok((needle.to_string(), None));
        }
        let all = items_of(&self.client.get("/workspaces", &[])?);
        let w = find(&all, needle).ok_or_else(|| {
            anyhow!("no workspace named \"{needle}\" is accessible with this key")
        })?;
        Ok((id_of(w).context("workspace record has no id")?, name_of(w)))
    }

    pub fn resolve_project(&self, ws: &str, needle: &str) -> Result<(String, Option<String>)> {
        if is_id(needle) {
            return Ok((needle.to_string(), None));
        }
        let all = items_of(
            &self
                .client
                .get(&format!("/workspaces/{}/projects", seg(ws)), &[])?,
        );
        let p = find(&all, needle)
            .ok_or_else(|| anyhow!("no project named \"{needle}\" in this workspace"))?;
        Ok((id_of(p).context("project record has no id")?, name_of(p)))
    }

    pub fn pick_workspace(&self) -> Result<(String, Option<String>)> {
        let all = items_of(&self.client.get("/workspaces", &[])?);
        pick(&all, "Workspace", "workspaces")
    }

    pub fn pick_project(&self, ws: &str) -> Result<(String, Option<String>)> {
        let all = items_of(
            &self
                .client
                .get(&format!("/workspaces/{}/projects", seg(ws)), &[])?,
        );
        if all.is_empty() {
            bail!(
                "this workspace has no projects yet — create one with `fenders projects create <name>`"
            );
        }
        pick(&all, "Project", "projects")
    }

    /// Resolve an app name (or id) to its id.
    pub fn app_id(&mut self, app: &str) -> Result<String> {
        if is_id(app) {
            return Ok(app.to_string());
        }
        let path = format!("{}/apps", self.proj_path()?);
        let page = self.client.get(
            &path,
            &[
                ("search".into(), app.into()),
                ("limit".into(), "100".into()),
            ],
        )?;
        let items = items_of(&page);
        find(&items, app).and_then(id_of).ok_or_else(|| {
            anyhow!("no app named \"{app}\" in this project (see `fenders apps list`)")
        })
    }

    pub fn app_path(&mut self, app: &str) -> Result<String> {
        let id = self.app_id(app)?;
        Ok(format!("{}/apps/{}", self.proj_path()?, seg(&id)))
    }

    /// Resolve a name among a project collection that returns a bare array
    /// (connections, secrets, artifacts).
    pub fn resolve_in(&mut self, collection: &str, needle: &str, noun: &str) -> Result<String> {
        if is_id(needle) {
            return Ok(needle.to_string());
        }
        let path = format!("{}/{collection}", self.proj_path()?);
        let items = items_of(&self.client.get(&path, &[])?);
        find(&items, needle)
            .and_then(id_of)
            .ok_or_else(|| anyhow!("no {noun} named \"{needle}\" in this project"))
    }

    /// Fetch up to `want` items (all when `None`) following the page cursor.
    /// Returns the items and whether more exist.
    pub fn collect(
        &self,
        path: &str,
        mut query: Vec<(String, String)>,
        cursor_param: &str,
        page_max: Option<usize>,
        want: Option<usize>,
    ) -> Result<(Vec<Value>, bool)> {
        let mut out: Vec<Value> = Vec::new();
        let mut last_cursor: Option<String> = None;
        for _ in 0..1000 {
            query.retain(|(k, _)| k != "limit");
            if let Some(max) = page_max {
                let n = want.map(|w| (w - out.len()).min(max)).unwrap_or(max);
                query.push(("limit".into(), n.to_string()));
            }
            let page = self.client.get(path, &query)?;
            out.extend(items_of(&page));
            let next = cursor_of(&page);
            let done = want.is_some_and(|w| out.len() >= w);
            match next {
                Some(c) if !done && last_cursor.as_deref() != Some(c.as_str()) => {
                    query.retain(|(k, _)| k != cursor_param);
                    query.push((cursor_param.into(), c.clone()));
                    last_cursor = Some(c);
                }
                Some(_) if done => {
                    if let Some(w) = want {
                        out.truncate(w);
                    }
                    return Ok((out, true));
                }
                _ => {
                    let more = want.is_some_and(|w| out.len() > w);
                    if let Some(w) = want {
                        out.truncate(w);
                    }
                    return Ok((out, more));
                }
            }
        }
        Ok((out, true))
    }

    /// Poll an operation until it leaves `pending`. Fails on `failed`.
    pub fn wait_op(&mut self, op: &Value, label: &str, timeout: Duration) -> Result<Value> {
        let state = |v: &Value| {
            v.get("state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let Some(id) = id_of(op) else {
            return Ok(op.clone());
        };
        let mut cur = op.clone();
        if state(&cur) == "pending" {
            let path = format!("{}/operations/{}", self.proj_path()?, seg(&id));
            let pb = ui::spinner(label.to_string());
            let started = Instant::now();
            let mut delay = Duration::from_millis(400);
            while state(&cur) == "pending" {
                if started.elapsed() > timeout {
                    pb.finish_and_clear();
                    bail!(
                        "timed out after {}s waiting for operation {id}; it is still running — check it with `fenders ops wait {id}`",
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(delay);
                delay = (delay * 3 / 2).min(Duration::from_secs(3));
                cur = self.client.get(&path, &[])?;
            }
            pb.finish_and_clear();
        }
        match state(&cur).as_str() {
            "failed" => {
                let why = cur
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason given");
                Err(anyhow!("{label} failed: {why}").context(format!("operation {id}")))
            }
            _ => {
                ui::success(&format!("{label} — done"));
                Ok(cur)
            }
        }
    }

    /// Either wait for an operation or print it and explain how to follow up.
    pub fn finish_op(
        &mut self,
        op: Value,
        label: &str,
        w: &crate::cmd::WaitArgs,
    ) -> Result<Option<Value>> {
        if w.no_wait {
            self.out.record(&op);
            if let Some(id) = id_of(&op) {
                ui::hint(&format!("Follow it with `fenders ops wait {id}`"));
            }
            return Ok(None);
        }
        self.wait_op(&op, label, Duration::from_secs(w.timeout))
            .map(Some)
    }

    /// The revision to send as `expected_revision`, read from `record_path`.
    pub fn revision(&self, record_path: &str, explicit: Option<i64>) -> Result<i64> {
        if let Some(r) = explicit {
            return Ok(r);
        }
        let rec = self.client.get(record_path, &[])?;
        revision_of(&rec).ok_or_else(|| {
            anyhow!(
                "could not find a revision on {record_path}; pass --expected-revision explicitly (fields: {})",
                rec.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default()
            )
        })
    }

    pub fn me(&self) -> Result<Value> {
        self.client.get("/me", &[])
    }
}

fn missing(what: &str, flag: &str, env: &str) -> String {
    format!("no {what} selected — run `fenders use`, pass {flag}, or set {env}")
}

fn pick(items: &[Value], prompt: &str, noun: &str) -> Result<(String, Option<String>)> {
    match items.len() {
        0 => bail!("no {noun} are accessible with this API key"),
        1 => {
            let v = &items[0];
            ui::info(&format!(
                "Using {} {}",
                prompt.to_lowercase(),
                ui::bold(&label(v), To::Err)
            ));
            Ok((id_of(v).context("record has no id")?, name_of(v)))
        }
        _ => {
            let labels: Vec<String> = items.iter().map(label).collect();
            let i = dialoguer::Select::with_theme(&ui::theme())
                .with_prompt(prompt)
                .items(&labels)
                .default(0)
                .interact()?;
            let v = &items[i];
            Ok((id_of(v).context("record has no id")?, name_of(v)))
        }
    }
}

/// Build a JSON object, dropping `None`/null fields.
pub fn compact(v: Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.into_iter().filter(|(_, v)| !v.is_null()).collect()),
        other => other,
    }
}
