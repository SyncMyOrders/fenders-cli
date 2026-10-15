//! Apps and everything hanging off them: versions, releases, runs, triggers,
//! connections and events.

use anyhow::{Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use super::{ConfigArgs, ListArgs, WaitArgs, artifact_ref};
use crate::app::{App, compact, id_of};
use crate::client::seg;
use crate::ui::{self, To};

const APP_COLS: &[&str] = &[
    "id", "name", "status", "state", "desired", "triggers", "updated",
];

#[derive(Subcommand, Debug)]
pub enum AppsCmd {
    /// List apps in the current project
    #[command(visible_alias = "ls")]
    List {
        /// Filter by name
        #[arg(long)]
        search: Option<String>,
        /// Filter by status
        #[arg(long)]
        status: Option<String>,
        /// Filter by trigger (http, manual, schedule)
        #[arg(long)]
        trigger: Option<String>,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show an app and its readiness
    #[command(visible_alias = "show")]
    Get { app: String },
    /// Create an app draft from an artifact (id, name or .wasm path)
    Create {
        name: String,
        #[arg(long, value_name = "ARTIFACT|FILE")]
        artifact: String,
        #[command(flatten)]
        config: ConfigArgs,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Start (enable) an app
    Start {
        app: String,
        #[arg(long, value_name = "N")]
        expected_revision: Option<i64>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Stop an app
    Stop {
        app: String,
        #[arg(long, value_name = "N")]
        expected_revision: Option<i64>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Show the app's HTTP address and status
    Url { app: String },
    /// Show the external connections an app may use
    Connections { app: String },
    /// Replace the app's connections
    SetConnections {
        app: String,
        /// Connections by name or id, optionally with an env prefix
        #[arg(value_name = "CONNECTION[:PREFIX]")]
        connections: Vec<String>,
        #[arg(long, value_name = "N")]
        expected_revision: Option<i64>,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum VersionsCmd {
    /// List an app's versions
    #[command(visible_alias = "ls")]
    List {
        app: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show a version
    Get { app: String, version: String },
    /// Create a version from an artifact (id, name or .wasm path)
    Create {
        app: String,
        #[arg(long, value_name = "ARTIFACT|FILE")]
        artifact: String,
        #[arg(long, value_name = "N")]
        expected_revision: Option<i64>,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum ReleasesCmd {
    /// List an app's releases
    #[command(visible_alias = "ls")]
    List {
        app: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show a release
    Get { app: String, release: String },
    /// Release a version, optionally with new configuration
    Create {
        app: String,
        #[arg(long = "version", value_name = "VERSION")]
        version_id: String,
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long, value_name = "N")]
        expected_revision: Option<i64>,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum RunSource {
    Manual,
    TestSchedule,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    pub app: String,
    /// What the run should be attributed to
    #[arg(long, value_enum, default_value = "manual")]
    pub source: RunSource,
    /// Print the run's logs when it finishes
    #[arg(long)]
    pub logs: bool,
    #[command(flatten)]
    pub wait: WaitArgs,
}

#[derive(Subcommand, Debug)]
pub enum RunsCmd {
    /// List an app's runs
    #[command(visible_alias = "ls")]
    List {
        app: String,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Show a run
    Get { app: String, run: String },
    /// Start a run (same as `fenders run`)
    Create(RunArgs),
}

#[derive(Subcommand, Debug)]
pub enum TriggersCmd {
    /// Show an app's triggers
    Get { app: String },
    /// Change an app's triggers
    Set {
        app: String,
        /// Expose the app over HTTP
        #[arg(long, value_name = "true|false")]
        http: Option<bool>,
        /// Allow manual runs
        #[arg(long, value_name = "true|false")]
        manual: Option<bool>,
        /// Run on a cron schedule
        #[arg(long, value_name = "CRON")]
        schedule: Option<String>,
        /// Time zone for the schedule
        #[arg(long, default_value = "UTC", requires = "schedule")]
        timezone: String,
        /// Disable the schedule
        #[arg(long, conflicts_with = "schedule")]
        no_schedule: bool,
        #[arg(long, value_name = "N")]
        expected_revision: Option<i64>,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Args, Debug)]
pub struct EventsArgs {
    pub app: String,
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub release: Option<String>,
    #[arg(long)]
    pub execution: Option<String>,
    #[command(flatten)]
    pub list: ListArgs,
}

pub fn apps(app: &mut App, cmd: AppsCmd) -> Result<()> {
    match cmd {
        AppsCmd::List {
            search,
            status,
            trigger,
            list,
        } => {
            let path = format!("{}/apps", app.proj_path()?);
            let mut q = vec![];
            for (k, v) in [("search", search), ("status", status), ("trigger", trigger)] {
                if let Some(v) = v {
                    q.push((k.to_string(), v));
                }
            }
            let (items, more) = app.collect(&path, q, "before", Some(100), list.want(&app.out))?;
            app.out.list(&items, APP_COLS, "apps");
            list.more_hint(more, items.len());
        }
        AppsCmd::Get { app: a } => {
            let p = app.app_path(&a)?;
            let rec = app.client.get(&p, &[])?;
            app.out.record(&rec);
        }
        AppsCmd::Create {
            name,
            artifact,
            config,
            wait,
        } => {
            let artifact_id = artifact_ref(app, &artifact)?;
            let body = compact(json!({
                "name": name,
                "artifact_id": artifact_id,
                "configuration": config.to_json(app)?,
            }));
            let path = format!("{}/apps", app.proj_path()?);
            let op = app.client.post(&path, body)?;
            if let Some(done) = app.finish_op(op, &format!("Create app {name}"), &wait)? {
                show_resource(app, &done, "apps")?;
            }
        }
        AppsCmd::Start {
            app: a,
            expected_revision,
            wait,
        } => lifecycle(app, &a, "enabled", expected_revision, &wait)?,
        AppsCmd::Stop {
            app: a,
            expected_revision,
            wait,
        } => lifecycle(app, &a, "stopped", expected_revision, &wait)?,
        AppsCmd::Url { app: a } => {
            let p = app.app_path(&a)?;
            let rec = app.client.get(&format!("{p}/http"), &[])?;
            match http_url(&rec) {
                Some(u) if !app.out.json => println!("{u}"),
                _ => app.out.record(&rec),
            }
        }
        AppsCmd::Connections { app: a } => {
            let p = app.app_path(&a)?;
            let rec = app.client.get(&format!("{p}/connections"), &[])?;
            app.out
                .auto(&rec, &["connection_id", "prefix", "name"], "connections");
        }
        AppsCmd::SetConnections {
            app: a,
            connections,
            expected_revision,
            wait,
        } => {
            let mut sel = vec![];
            for c in &connections {
                let (name, prefix) = match c.split_once(':') {
                    Some((n, p)) => (n, Some(p)),
                    None => (c.as_str(), None),
                };
                let id = app.resolve_in("connections", name, "connection")?;
                sel.push(compact(json!({ "connection_id": id, "prefix": prefix })));
            }
            let p = app.app_path(&a)?;
            let rev = revision_for(app, &format!("{p}/connections"), &p, expected_revision)?;
            let op = app.client.put(
                &format!("{p}/connections"),
                json!({ "connections": sel, "expected_revision": rev }),
            )?;
            app.finish_op(op, "Update connections", &wait)?;
        }
    }
    Ok(())
}

fn lifecycle(
    app: &mut App,
    a: &str,
    desired: &str,
    rev: Option<i64>,
    wait: &WaitArgs,
) -> Result<()> {
    let p = app.app_path(a)?;
    let rev = app.revision(&p, rev)?;
    let op = app
        .client
        .patch(&p, json!({ "desired": desired, "expected_revision": rev }))?;
    let verb = if desired == "enabled" {
        "Start"
    } else {
        "Stop"
    };
    app.finish_op(op, &format!("{verb} {a}"), wait)?;
    Ok(())
}

/// Revision from a sub-record (e.g. triggers), falling back to the app's.
fn revision_for(app: &App, sub: &str, app_path: &str, explicit: Option<i64>) -> Result<i64> {
    if let Some(r) = explicit {
        return Ok(r);
    }
    if let Some(r) = app
        .client
        .get(sub, &[])
        .ok()
        .as_ref()
        .and_then(crate::app::revision_of)
    {
        return Ok(r);
    }
    app.revision(app_path, None)
}

/// After an operation completes, show the resource it produced.
fn show_resource(app: &mut App, op: &Value, collection: &str) -> Result<()> {
    match op.get("resource_id").and_then(Value::as_str) {
        Some(rid) => {
            let path = format!("{}/{collection}/{}", app.proj_path()?, seg(rid));
            match app.client.get(&path, &[]) {
                Ok(rec) => app.out.record(&rec),
                Err(_) => app.out.record(op),
            }
        }
        None => app.out.record(op),
    }
    Ok(())
}

pub fn http_url(rec: &Value) -> Option<String> {
    ["url", "address", "endpoint", "hostname", "host"]
        .iter()
        .find_map(|k| {
            let s = rec.get(*k)?.as_str()?;
            Some(if s.contains("://") {
                s.to_string()
            } else {
                format!("https://{s}")
            })
        })
}

pub fn versions(app: &mut App, cmd: VersionsCmd) -> Result<()> {
    match cmd {
        VersionsCmd::List { app: a, list } => {
            let p = format!("{}/versions", app.app_path(&a)?);
            let (items, more) = app.collect(&p, vec![], "before", None, list.want(&app.out))?;
            app.out.list(
                &items,
                &["id", "number", "version", "artifact_id", "created"],
                "versions",
            );
            list.more_hint(more, items.len());
        }
        VersionsCmd::Get { app: a, version } => {
            let p = format!("{}/versions/{}", app.app_path(&a)?, seg(&version));
            let rec = app.client.get(&p, &[])?;
            app.out.record(&rec);
        }
        VersionsCmd::Create {
            app: a,
            artifact,
            expected_revision,
            wait,
        } => {
            let artifact_id = artifact_ref(app, &artifact)?;
            let p = app.app_path(&a)?;
            let rev = app.revision(&p, expected_revision)?;
            let op = app.client.post(
                &format!("{p}/versions"),
                json!({ "artifact_id": artifact_id, "expected_revision": rev }),
            )?;
            if let Some(done) = app.finish_op(op, "Create version", &wait)? {
                app.out.record(&done);
            }
        }
    }
    Ok(())
}

pub fn releases(app: &mut App, cmd: ReleasesCmd) -> Result<()> {
    match cmd {
        ReleasesCmd::List { app: a, list } => {
            let p = format!("{}/releases", app.app_path(&a)?);
            let (items, more) =
                app.collect(&p, vec![], "before", Some(100), list.want(&app.out))?;
            app.out.list(
                &items,
                &["id", "version_id", "state", "status", "created"],
                "releases",
            );
            list.more_hint(more, items.len());
        }
        ReleasesCmd::Get { app: a, release } => {
            let p = format!("{}/releases/{}", app.app_path(&a)?, seg(&release));
            let rec = app.client.get(&p, &[])?;
            app.out.record(&rec);
        }
        ReleasesCmd::Create {
            app: a,
            version_id,
            config,
            expected_revision,
            wait,
        } => {
            let p = app.app_path(&a)?;
            let rev = app.revision(&p, expected_revision)?;
            let body = compact(json!({
                "version_id": version_id,
                "configuration": config.to_json(app)?,
                "expected_revision": rev,
            }));
            let op = app.client.post(&format!("{p}/releases"), body)?;
            if let Some(done) = app.finish_op(op, "Release", &wait)? {
                app.out.record(&done);
            }
        }
    }
    Ok(())
}

pub fn run(app: &mut App, args: RunArgs) -> Result<()> {
    let p = app.app_path(&args.app)?;
    let source = match args.source {
        RunSource::Manual => "manual",
        RunSource::TestSchedule => "test-schedule",
    };
    let op = app
        .client
        .post(&format!("{p}/runs"), json!({ "source": source }))?;
    let Some(done) = app.finish_op(op, &format!("Run {}", args.app), &args.wait)? else {
        return Ok(());
    };
    let run_id = done
        .get("resource_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    match &run_id {
        Some(rid) => {
            let rec = app.client.get(&format!("{p}/runs/{}", seg(rid)), &[])?;
            app.out.record(&rec);
        }
        None => app.out.record(&done),
    }
    if args.logs {
        if let Some(rid) = run_id {
            if !app.out.json {
                println!();
            }
            super::logs::print_for_run(app, &p, &rid)?;
        }
    } else if let (Some(rid), false) = (run_id, app.out.json) {
        ui::hint(&format!("Logs: fenders logs {} --run {rid}", args.app));
    }
    Ok(())
}

pub fn runs(app: &mut App, cmd: RunsCmd) -> Result<()> {
    match cmd {
        RunsCmd::List { app: a, list } => {
            let p = format!("{}/runs", app.app_path(&a)?);
            let (items, more) =
                app.collect(&p, vec![], "before", Some(100), list.want(&app.out))?;
            app.out.list(
                &items,
                &["id", "source", "state", "status", "created", "duration_ms"],
                "runs",
            );
            list.more_hint(more, items.len());
        }
        RunsCmd::Get { app: a, run } => {
            let p = format!("{}/runs/{}", app.app_path(&a)?, seg(&run));
            let rec = app.client.get(&p, &[])?;
            app.out.record(&rec);
        }
        RunsCmd::Create(args) => run(app, args)?,
    }
    Ok(())
}

pub fn triggers(app: &mut App, cmd: TriggersCmd) -> Result<()> {
    match cmd {
        TriggersCmd::Get { app: a } => {
            let p = app.app_path(&a)?;
            let rec = app.client.get(&format!("{p}/triggers"), &[])?;
            app.out.record(&rec);
        }
        TriggersCmd::Set {
            app: a,
            http,
            manual,
            schedule,
            timezone,
            no_schedule,
            expected_revision,
            wait,
        } => {
            if http.is_none() && manual.is_none() && schedule.is_none() && !no_schedule {
                bail!("nothing to change — pass --http, --manual, --schedule or --no-schedule");
            }
            let schedule = if no_schedule {
                Some(json!({ "enabled": false }))
            } else {
                schedule.map(|e| json!({ "enabled": true, "expression": e, "timezone": timezone }))
            };
            let p = app.app_path(&a)?;
            let rev = revision_for(app, &format!("{p}/triggers"), &p, expected_revision)?;
            let body = compact(json!({
                "http": http,
                "manual": manual,
                "schedule": schedule,
                "expected_revision": rev,
            }));
            let op = app.client.put(&format!("{p}/triggers"), body)?;
            if app.finish_op(op, "Update triggers", &wait)?.is_some() {
                let rec = app.client.get(&format!("{p}/triggers"), &[])?;
                app.out.record(&rec);
            }
        }
    }
    Ok(())
}

pub fn events(app: &mut App, args: EventsArgs) -> Result<()> {
    let p = format!("{}/events", app.app_path(&args.app)?);
    let mut q = vec![];
    for (k, v) in [
        ("run_id", args.run),
        ("release_id", args.release),
        ("execution_id", args.execution),
    ] {
        if let Some(v) = v {
            q.push((k.to_string(), v));
        }
    }
    let (items, more) = app.collect(&p, q, "before", Some(100), args.list.want(&app.out))?;
    app.out.list(
        &items,
        &[
            "created", "time", "type", "kind", "level", "message", "run_id",
        ],
        "events",
    );
    args.list.more_hint(more, items.len());
    Ok(())
}

/// Summary printed after a deploy/release.
pub fn summary(app: &mut App, app_id: &str, name: &str) -> Result<()> {
    let p = format!("{}/apps/{}", app.proj_path()?, seg(app_id));
    let rec = app.client.get(&p, &[]).unwrap_or(Value::Null);
    let http = app.client.get(&format!("{p}/http"), &[]).ok();
    let url = http.as_ref().and_then(http_url);
    if app.out.json {
        app.out.print_json(&json!({ "app": rec, "http": http }));
        return Ok(());
    }
    let row = |k: &str, v: String| println!("  {}  {v}", ui::dim(&format!("{k:<8}"), To::Out));
    println!();
    row(
        "app",
        format!("{} {}", ui::bold(name, To::Out), ui::dim(app_id, To::Out)),
    );
    for k in ["status", "state", "desired"] {
        if let Some(s) = rec.get(k).and_then(Value::as_str) {
            row(k, ui::state(s, To::Out));
        }
    }
    if let Some(u) = url {
        row("url", ui::accent(&u, To::Out));
    }
    if let Some(r) = id_of(&rec.get("release").cloned().unwrap_or(Value::Null)) {
        row("release", r);
    }
    println!();
    ui::hint(&format!("Logs:   fenders logs {name} -f"));
    ui::hint(&format!("Run it: fenders run {name}"));
    Ok(())
}
