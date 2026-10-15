//! `fenders deploy app.wasm` / `fenders apps deploy NAME --artifact app.wasm`
//! — upload, validate, deploy, wait, show the URL.
//!
//! A new name creates and deploys an app in one step. An existing app gets a
//! new version and a release of it. Triggers default to what the component
//! supports: an HTTP component gets an HTTP endpoint without extra flags.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use serde_json::{Value, json};

use super::{ConfigArgs, WaitArgs, check_inspection, upload_wasm};
use crate::app::{App, compact, find, id_of, items_of};
use crate::client::seg;
use crate::ui::{self, To};

/// Options shared by `fenders deploy` and `fenders apps deploy`.
#[derive(Args, Debug, Clone)]
pub struct DeployOpts {
    /// Expose the app over HTTP (default: on when the component handles HTTP)
    #[arg(long, overrides_with = "no_http")]
    pub http: bool,
    /// Don't expose the app over HTTP
    #[arg(long)]
    pub no_http: bool,
    /// Allow manual runs (default: on when the component is a command)
    #[arg(long, overrides_with = "no_manual")]
    pub manual: bool,
    /// Disallow manual runs
    #[arg(long)]
    pub no_manual: bool,
    /// Run on a cron schedule
    #[arg(long, value_name = "CRON")]
    pub schedule: Option<String>,
    /// Time zone for --schedule
    #[arg(long, default_value = "UTC", requires = "schedule")]
    pub timezone: String,
    /// Allow an external connection (name or id), optionally with an env prefix
    #[arg(long = "connection", value_name = "CONNECTION[:PREFIX]")]
    pub connections: Vec<String>,
    /// Deploy even if artifact inspection reports problems
    #[arg(long)]
    pub skip_inspection: bool,
    #[command(flatten)]
    pub config: ConfigArgs,
    #[command(flatten)]
    pub wait: WaitArgs,
}

#[derive(Args, Debug)]
pub struct DeployArgs {
    /// The WASI component to deploy
    #[arg(value_name = "FILE.wasm")]
    pub file: PathBuf,
    /// App name (defaults to the file name)
    #[arg(long)]
    pub name: Option<String>,
    /// Release to this existing app (name or id) instead of creating one
    #[arg(long, conflicts_with = "name")]
    pub app: Option<String>,
    #[command(flatten)]
    pub opts: DeployOpts,
}

#[derive(Args, Debug)]
pub struct AppsDeployArgs {
    /// App name; an existing app gets a new release
    pub name: String,
    /// The component: a .wasm file, or an uploaded artifact's id
    #[arg(long, value_name = "FILE|ARTIFACT")]
    pub artifact: String,
    #[command(flatten)]
    pub opts: DeployOpts,
}

pub fn run(app: &mut App, args: DeployArgs) -> Result<()> {
    if !args.file.is_file() {
        bail!("{} does not exist or is not a file", args.file.display());
    }
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| default_name(&args.file));
    let source = args.file.display().to_string();
    deploy(app, &source, &name, args.app.as_deref(), &args.opts)
}

pub fn run_apps(app: &mut App, args: AppsDeployArgs) -> Result<()> {
    deploy(app, &args.artifact, &args.name, None, &args.opts)
}

/// `source` is a .wasm path or an artifact id; `existing` an explicit
/// existing app to release to.
fn deploy(
    app: &mut App,
    source: &str,
    name: &str,
    existing: Option<&str>,
    opts: &DeployOpts,
) -> Result<()> {
    let target = match existing {
        Some(a) => Some((app.app_id(a)?, a.to_string())),
        None => find_existing(app, name)?.map(|id| (id, name.to_string())),
    };

    let artifact_id = if Path::new(source).is_file() {
        let rec = upload_wasm(app, Path::new(source))?;
        id_of(&rec).context("artifact has no id")?
    } else {
        app.resolve_in("artifacts", source, "artifact")
            .with_context(|| format!("{source} is neither a file nor an uploaded artifact"))?
    };
    let caps = inspect(app, &artifact_id, opts.skip_inspection)?;

    match target {
        Some((app_id, name)) => release_existing(app, opts, &app_id, &name, &artifact_id),
        None => deploy_new(app, opts, name, &artifact_id, caps),
    }
}

/// An app with exactly this name, after confirming the user wants to release to it.
fn find_existing(app: &mut App, name: &str) -> Result<Option<String>> {
    let proj = app.proj_path()?;
    let page = app.client.get(
        &format!("{proj}/apps"),
        &[
            ("search".into(), name.to_string()),
            ("limit".into(), "100".into()),
        ],
    )?;
    let items = items_of(&page);
    let Some(v) = find(&items, name).filter(|v| crate::app::name_of(v).as_deref() == Some(name))
    else {
        return Ok(None);
    };
    let id = id_of(v).context("app record has no id")?;
    let go = ui::interactive()
        && dialoguer::Confirm::with_theme(&ui::theme())
            .with_prompt(format!(
                "App \"{name}\" already exists. Release this build to it?"
            ))
            .default(true)
            .interact()?;
    if !go {
        bail!(
            "an app named \"{name}\" already exists — pass `--app {name}` to `fenders deploy` to release to it, or choose another name"
        );
    }
    Ok(Some(id))
}

fn default_name(file: &Path) -> String {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "app".into());
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// What a component supports, from its inspection.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Capabilities {
    pub http: Option<bool>,
    pub command: Option<bool>,
}

pub fn capabilities(insp: &Value) -> Capabilities {
    let get = |k: &str| {
        insp.get("inspection")
            .and_then(|i| i.get(k))
            .or_else(|| insp.get(k))
            .and_then(Value::as_bool)
    };
    Capabilities {
        http: get("http"),
        command: get("command"),
    }
}

/// HTTP and manual triggers: explicit flags win, otherwise follow what the
/// component supports.
pub fn triggers_for(opts: &DeployOpts, caps: Capabilities) -> (bool, bool) {
    let http = if opts.http {
        true
    } else if opts.no_http {
        false
    } else {
        caps.http.unwrap_or(false)
    };
    let manual = if opts.manual {
        true
    } else if opts.no_manual {
        false
    } else {
        caps.command.unwrap_or(!http)
    };
    (http, manual)
}

fn inspect(app: &mut App, artifact_id: &str, skip: bool) -> Result<Capabilities> {
    let path = format!(
        "{}/artifacts/{}/inspection",
        app.proj_path()?,
        seg(artifact_id)
    );
    let pb = ui::spinner("Inspecting component");
    let insp = app.client.get(&path, &[]);
    pb.finish_and_clear();
    match insp {
        Ok(insp) => {
            match check_inspection(&insp) {
                Some(problem) if !skip => {
                    if !app.out.json {
                        app.out.record(&insp);
                    }
                    bail!("{problem} (use --skip-inspection to deploy anyway)");
                }
                Some(problem) => ui::warn(&problem),
                None => {
                    let caps = capabilities(&insp);
                    let kinds: Vec<&str> = [(caps.http, "HTTP handler"), (caps.command, "command")]
                        .into_iter()
                        .filter_map(|(on, k)| (on == Some(true)).then_some(k))
                        .collect();
                    if kinds.is_empty() {
                        ui::success("Component inspected");
                    } else {
                        ui::success(&format!("Component inspected: {}", kinds.join(", ")));
                    }
                }
            }
            Ok(capabilities(&insp))
        }
        Err(e) => {
            ui::warn(&format!("could not inspect the component: {e}"));
            Ok(Capabilities::default())
        }
    }
}

fn deploy_new(
    app: &mut App,
    opts: &DeployOpts,
    name: &str,
    artifact_id: &str,
    caps: Capabilities,
) -> Result<()> {
    let mut conns = vec![];
    for c in &opts.connections {
        let (n, prefix) = match c.split_once(':') {
            Some((n, p)) => (n, Some(p)),
            None => (c.as_str(), None),
        };
        let id = app.resolve_in("connections", n, "connection")?;
        conns.push(compact(json!({ "connection_id": id, "prefix": prefix })));
    }
    let schedule = opts
        .schedule
        .as_ref()
        .map(|e| json!({ "enabled": true, "expression": e, "timezone": opts.timezone }));
    let (http, manual) = triggers_for(opts, caps);
    if caps.http == Some(false) && http {
        ui::warn("--http was given but the component doesn't export an HTTP handler.");
    }
    let body = compact(json!({
        "name": name,
        "artifact_id": artifact_id,
        "configuration": opts.config.to_json(app)?,
        "triggers": compact(json!({ "http": http, "manual": manual, "schedule": schedule })),
        "connections": if conns.is_empty() { Value::Null } else { Value::Array(conns) },
    }));
    let path = format!("{}/apps/deploy", app.proj_path()?);
    let op = app.client.post(&path, body)?;
    let Some(done) = app.finish_op(
        op,
        &format!("Deploy {}", ui::bold(name, To::Err)),
        &opts.wait,
    )?
    else {
        return Ok(());
    };
    let app_id = done
        .get("resource_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("deploy finished but returned no app id: {done}"))?
        .to_string();
    super::apps::summary(app, &app_id, name)
}

fn release_existing(
    app: &mut App,
    opts: &DeployOpts,
    app_id: &str,
    name: &str,
    artifact_id: &str,
) -> Result<()> {
    if opts.http
        || opts.no_http
        || opts.manual
        || opts.no_manual
        || opts.schedule.is_some()
        || !opts.connections.is_empty()
    {
        ui::warn(
            "Trigger and connection flags only apply to new apps; use `fenders triggers set` / `fenders apps set-connections`.",
        );
    }
    let p = format!("{}/apps/{}", app.proj_path()?, seg(app_id));
    let rev = app.revision(&p, None)?;
    let op = app.client.post(
        &format!("{p}/versions"),
        json!({ "artifact_id": artifact_id, "expected_revision": rev }),
    )?;
    // The release needs the new version's id, so always wait for the version;
    // --no-wait applies to the release, which is the step the user asked for.
    let version_wait = WaitArgs {
        no_wait: false,
        ..opts.wait.clone()
    };
    let Some(done) = app.finish_op(op, "Create version", &version_wait)? else {
        return Ok(());
    };
    let version_id = done
        .get("resource_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("version created but no id was returned: {done}"))?
        .to_string();

    let rev = app.revision(&p, None)?;
    let body = compact(json!({
        "version_id": version_id,
        "configuration": opts.config.to_json(app)?,
        "expected_revision": rev,
    }));
    let op = app.client.post(&format!("{p}/releases"), body)?;
    if app
        .finish_op(
            op,
            &format!("Release {}", ui::bold(name, To::Err)),
            &opts.wait,
        )?
        .is_none()
    {
        return Ok(());
    }
    super::apps::summary(app, app_id, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        opts: DeployOpts,
    }

    fn opts(args: &[&str]) -> DeployOpts {
        T::parse_from(std::iter::once("t").chain(args.iter().copied())).opts
    }

    #[test]
    fn names_from_files() {
        assert_eq!(
            default_name(Path::new("target/wasm32-wasip2/release/hello_world.wasm")),
            "hello-world"
        );
        assert_eq!(default_name(Path::new("My App.wasm")), "my-app");
    }

    #[test]
    fn reads_capabilities_from_real_inspection_shape() {
        let insp = json!({"artifact_id": "x", "error": null, "inspection": {"http": true, "command": false}});
        assert_eq!(
            capabilities(&insp),
            Capabilities {
                http: Some(true),
                command: Some(false)
            }
        );
    }

    #[test]
    fn triggers_follow_the_component_unless_overridden() {
        let http_component = Capabilities {
            http: Some(true),
            command: Some(false),
        };
        let command_component = Capabilities {
            http: Some(false),
            command: Some(true),
        };
        assert_eq!(triggers_for(&opts(&[]), http_component), (true, false));
        assert_eq!(triggers_for(&opts(&[]), command_component), (false, true));
        assert_eq!(
            triggers_for(&opts(&["--no-http"]), http_component),
            (false, false)
        );
        assert_eq!(
            triggers_for(&opts(&["--manual"]), http_component),
            (true, true)
        );
        assert_eq!(
            triggers_for(&opts(&[]), Capabilities::default()),
            (false, true)
        );
    }
}
