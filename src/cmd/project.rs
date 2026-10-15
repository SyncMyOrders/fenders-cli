//! Workspace- and project-level resources: workspaces, projects, usage,
//! sizes, HTTPS access, schedules, artifacts, connections, secrets, operations.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use super::{ListArgs, WaitArgs, check_inspection, confirm, read_secret, upload_wasm};
use crate::app::{App, id_of, name_of};
use crate::client::seg;
use crate::ui;

#[derive(Subcommand, Debug)]
pub enum WorkspacesCmd {
    /// List workspaces this key can access
    #[command(visible_alias = "ls")]
    List,
}

#[derive(Subcommand, Debug)]
pub enum ProjectsCmd {
    /// List projects in the current workspace
    #[command(visible_alias = "ls")]
    List,
    /// Create a project
    Create {
        /// 2–40 characters
        name: String,
        /// Switch to the new project
        #[arg(long = "use")]
        use_it: bool,
    },
}

#[derive(Args, Debug)]
pub struct UsageArgs {
    /// Whole workspace instead of the current project
    #[arg(long)]
    pub workspace_wide: bool,
    /// Monthly history instead of the current period
    #[arg(long)]
    pub history: bool,
    #[command(flatten)]
    pub list: ListArgs,
}

#[derive(Subcommand, Debug)]
pub enum HttpsCmd {
    /// Show the project's outbound HTTPS destination policy
    Show,
    /// Check whether origins would be allowed
    Validate {
        #[arg(required = true, value_name = "ORIGIN")]
        origins: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ScheduleCmd {
    /// Show when a cron expression would fire
    Preview {
        #[arg(value_name = "CRON")]
        expression: String,
        #[arg(long, default_value = "UTC")]
        timezone: String,
        /// Start from this RFC 3339 time instead of now
        #[arg(long)]
        after: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ArtifactsCmd {
    /// List uploaded components
    #[command(visible_alias = "ls")]
    List,
    /// Show an artifact
    Get { artifact: String },
    /// Show validation results and supported interfaces
    Inspect { artifact: String },
    /// Upload a WASI component (identical bytes reuse the existing artifact)
    Upload {
        #[arg(value_name = "FILE.wasm")]
        file: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConnectionsCmd {
    /// List external connections
    #[command(visible_alias = "ls")]
    List,
    /// Show a connection
    Get { connection: String },
    /// Register an external connection
    #[command(subcommand)]
    Create(NewConnection),
    /// Revoke an external connection
    Revoke {
        connection: String,
        #[arg(short, long)]
        yes: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum NewConnection {
    /// An HTTPS origin, e.g. https://api.example.com
    Https {
        name: String,
        #[arg(long)]
        origin: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// A raw TCP endpoint
    Tcp {
        name: String,
        #[arg(long)]
        host: String,
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum SecretsCmd {
    /// List secrets (values are never shown)
    #[command(visible_alias = "ls")]
    List,
    /// Show a secret's metadata
    Get { secret: String },
    /// Create an encrypted secret; the value is read from stdin or a hidden prompt
    Create {
        name: String,
        /// Read the value from a file instead
        #[arg(long, value_name = "PATH")]
        from_file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum OpsCmd {
    /// List recent operations
    #[command(visible_alias = "ls")]
    List,
    /// Show an operation
    Get { operation: String },
    /// Wait for an operation to finish
    Wait {
        operation: String,
        #[arg(long, default_value_t = 600, value_name = "SECS")]
        timeout: u64,
    },
}

pub fn workspaces(app: &mut App, cmd: WorkspacesCmd) -> Result<()> {
    match cmd {
        WorkspacesCmd::List => {
            let v = app.client.get("/workspaces", &[])?;
            let items = mark_current(
                crate::app::items_of(&v),
                app.cfg.workspace.as_deref(),
                app.out.json,
            );
            app.out
                .list(&items, &["current", "id", "name", "role"], "workspaces");
        }
    }
    Ok(())
}

/// Add a "●" column marking the selected item (table output only).
fn mark_current(mut items: Vec<Value>, current: Option<&str>, json: bool) -> Vec<Value> {
    if json || current.is_none() {
        return items;
    }
    for i in items.iter_mut() {
        let here = id_of(i).as_deref() == current;
        if let Some(o) = i.as_object_mut() {
            o.insert("current".into(), json!(if here { "●" } else { "" }));
        }
    }
    items
}

pub fn projects(app: &mut App, cmd: ProjectsCmd) -> Result<()> {
    match cmd {
        ProjectsCmd::List => {
            let p = format!("{}/projects", app.ws_path()?);
            let v = app.client.get(&p, &[])?;
            let current = app.cfg.project.clone();
            let items = mark_current(crate::app::items_of(&v), current.as_deref(), app.out.json);
            app.out.list(&items, &["current", "id", "name"], "projects");
        }
        ProjectsCmd::Create { name, use_it } => {
            if !(2..=40).contains(&name.chars().count()) {
                bail!("project names must be 2–40 characters");
            }
            let p = format!("{}/projects", app.ws_path()?);
            let rec = app.client.post(&p, json!({ "name": name }))?;
            ui::success(&format!("Created project {name}"));
            if use_it && let Some(id) = id_of(&rec) {
                let ws = app.workspace()?;
                if app.cfg.workspace.as_deref() != Some(ws.as_str()) {
                    app.cfg.set_workspace(ws, None);
                }
                app.cfg.set_project(id, name_of(&rec).or(Some(name)));
                app.cfg.save()?;
                ui::success("Switched to it");
            }
            app.out.record(&rec);
        }
    }
    Ok(())
}

pub fn usage(app: &mut App, args: UsageArgs) -> Result<()> {
    let base = if args.workspace_wide {
        app.ws_path()?
    } else {
        app.proj_path()?
    };
    if args.history {
        let (items, more) = app.collect(
            &format!("{base}/usage/history"),
            vec![],
            "before",
            Some(60),
            args.list.want(&app.out),
        )?;
        app.out.list(
            &items,
            &["month", "period", "period_start"],
            "usage records",
        );
        args.list.more_hint(more, items.len());
    } else {
        let rec = app.client.get(&format!("{base}/usage"), &[])?;
        app.out.record(&rec);
    }
    Ok(())
}

pub fn sizes(app: &mut App) -> Result<()> {
    let path = format!("{}/app-sizes", app.proj_path()?);
    let rec = app.client.get(&path, &[])?;
    app.out
        .auto(&rec, &["name", "size", "memory", "cpu", "default"], "sizes");
    Ok(())
}

pub fn https(app: &mut App, cmd: HttpsCmd) -> Result<()> {
    let p = format!("{}/https-access", app.proj_path()?);
    let rec = match cmd {
        HttpsCmd::Show => app.client.get(&p, &[])?,
        HttpsCmd::Validate { origins } => app
            .client
            .post(&format!("{p}/validate"), json!({ "origins": origins }))?,
    };
    app.out
        .auto(&rec, &["origin", "allowed", "valid", "reason"], "origins");
    Ok(())
}

pub fn schedule(app: &mut App, cmd: ScheduleCmd) -> Result<()> {
    match cmd {
        ScheduleCmd::Preview {
            expression,
            timezone,
            after,
        } => {
            let p = format!("{}/schedules/preview", app.proj_path()?);
            let mut body = json!({ "expression": expression, "timezone": timezone });
            if let Some(a) = after {
                body["after"] = json!(a);
            }
            let rec = app.client.post(&p, body)?;
            // Show a list of upcoming times plainly when that's what came back.
            let times = ["occurrences", "next", "times", "items"]
                .iter()
                .find_map(|k| rec.get(*k).and_then(Value::as_array))
                .filter(|a| a.iter().all(Value::is_string));
            match times {
                Some(t) if !app.out.json => {
                    for x in t {
                        let s = x.as_str().unwrap_or_default();
                        let rel = ui::relative_time(s).unwrap_or_default();
                        println!("{s}  {}", ui::dim(&rel, ui::To::Out));
                    }
                }
                _ => app.out.record(&rec),
            }
        }
    }
    Ok(())
}

pub fn artifacts(app: &mut App, cmd: ArtifactsCmd) -> Result<()> {
    let base = format!("{}/artifacts", app.proj_path()?);
    match cmd {
        ArtifactsCmd::List => {
            let v = app.client.get(&base, &[])?;
            app.out.list(
                &crate::app::items_of(&v),
                &["id", "name", "size", "digest", "created"],
                "artifacts",
            );
        }
        ArtifactsCmd::Get { artifact } => {
            let rec = app.client.get(&format!("{base}/{}", seg(&artifact)), &[])?;
            app.out.record(&rec);
        }
        ArtifactsCmd::Inspect { artifact } => {
            let rec = app
                .client
                .get(&format!("{base}/{}/inspection", seg(&artifact)), &[])?;
            app.out.record(&rec);
            if let Some(problem) = check_inspection(&rec) {
                ui::warn(&problem);
            }
        }
        ArtifactsCmd::Upload { file } => {
            let rec = upload_wasm(app, &file)?;
            app.out.record(&rec);
            if let Some(id) = id_of(&rec) {
                ui::hint(&format!("Check it with `fenders artifacts inspect {id}`"));
            }
        }
    }
    Ok(())
}

pub fn connections(app: &mut App, cmd: ConnectionsCmd) -> Result<()> {
    let base = format!("{}/connections", app.proj_path()?);
    match cmd {
        ConnectionsCmd::List => {
            let v = app.client.get(&base, &[])?;
            app.out.list(
                &crate::app::items_of(&v),
                &[
                    "id", "name", "type", "origin", "host", "port", "state", "status",
                ],
                "connections",
            );
        }
        ConnectionsCmd::Get { connection } => {
            let id = app.resolve_in("connections", &connection, "connection")?;
            let rec = app.client.get(&format!("{base}/{}", seg(&id)), &[])?;
            app.out.record(&rec);
        }
        ConnectionsCmd::Create(new) => {
            let (body, name, wait) = match new {
                NewConnection::Https { name, origin, wait } => {
                    if !origin.starts_with("https://") {
                        bail!("--origin must be an https:// URL");
                    }
                    (
                        json!({ "type": "https", "name": name, "origin": origin }),
                        name,
                        wait,
                    )
                }
                NewConnection::Tcp {
                    name,
                    host,
                    port,
                    wait,
                } => (
                    json!({ "type": "tcp", "name": name, "host": host, "port": port }),
                    name,
                    wait,
                ),
            };
            let op = app.client.post(&base, body)?;
            if let Some(done) = app.finish_op(op, &format!("Register connection {name}"), &wait)? {
                app.out.record(&done);
            }
        }
        ConnectionsCmd::Revoke {
            connection,
            yes,
            wait,
        } => {
            let id = app.resolve_in("connections", &connection, "connection")?;
            confirm(
                &format!("Revoke connection {connection}? Apps using it lose access."),
                yes,
            )?;
            let op = app.client.delete(&format!("{base}/{}", seg(&id)))?;
            app.finish_op(op, &format!("Revoke {connection}"), &wait)?;
        }
    }
    Ok(())
}

pub fn secrets(app: &mut App, cmd: SecretsCmd) -> Result<()> {
    let base = format!("{}/secrets", app.proj_path()?);
    match cmd {
        SecretsCmd::List => {
            let v = app.client.get(&base, &[])?;
            app.out.list(
                &crate::app::items_of(&v),
                &["id", "name", "created"],
                "secrets",
            );
        }
        SecretsCmd::Get { secret } => {
            let id = app.resolve_in("secrets", &secret, "secret")?;
            let rec = app.client.get(&format!("{base}/{}", seg(&id)), &[])?;
            app.out.record(&rec);
        }
        SecretsCmd::Create { name, from_file } => {
            let value = match from_file {
                Some(p) => std::fs::read_to_string(&p)?
                    .trim_end_matches(['\r', '\n'])
                    .to_string(),
                None => read_secret(&format!("Value for {name}"), true)?,
            };
            if value.is_empty() {
                bail!("the secret value is empty");
            }
            if value.len() > 1024 {
                bail!("secret values may be at most 1024 characters");
            }
            let rec = app
                .client
                .post(&base, json!({ "name": name, "value": value }))?;
            ui::success(&format!("Stored secret {name}"));
            app.out.record(&rec);
            ui::hint(&format!(
                "Use it with `--secret-env ENV_NAME={name}` on deploy or release"
            ));
        }
    }
    Ok(())
}

pub fn ops(app: &mut App, cmd: OpsCmd) -> Result<()> {
    let base = format!("{}/operations", app.proj_path()?);
    match cmd {
        OpsCmd::List => {
            let v = app.client.get(&base, &[])?;
            app.out.list(
                &crate::app::items_of(&v),
                &[
                    "id",
                    "kind",
                    "state",
                    "resource_id",
                    "error",
                    "created",
                    "updated",
                ],
                "operations",
            );
        }
        OpsCmd::Get { operation } => {
            let rec = app
                .client
                .get(&format!("{base}/{}", seg(&operation)), &[])?;
            app.out.record(&rec);
        }
        OpsCmd::Wait { operation, timeout } => {
            let op = app
                .client
                .get(&format!("{base}/{}", seg(&operation)), &[])?;
            let kind = op
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("operation")
                .to_string();
            let done = app.wait_op(&op, &kind, Duration::from_secs(timeout))?;
            app.out.record(&done);
        }
    }
    Ok(())
}
