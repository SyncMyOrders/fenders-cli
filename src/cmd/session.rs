//! `auth`, `use` and `context`: who you are and where commands apply.

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::app::{App, id_of, name_of};
use crate::auth;
use crate::client::{ApiError, Client};
use crate::ui::{self, To};

#[derive(Subcommand, Debug)]
pub enum AuthCmd {
    /// Log in with a workspace API key
    Login {
        /// Read the API key from stdin instead of prompting
        #[arg(long)]
        with_token: bool,
    },
    /// Forget the stored API key
    Logout,
    /// Show the current identity and where the key comes from
    Status,
    /// Print the API key (for use in scripts)
    Token,
}

#[derive(Args, Debug)]
pub struct UseArgs {
    /// WORKSPACE or WORKSPACE/PROJECT, by name or id. Prompts when omitted.
    pub target: Option<String>,
}

fn identity(me: &Value) -> String {
    ["email", "name", "key_name", "label"]
        .iter()
        .find_map(|k| me.get(*k).and_then(Value::as_str))
        .map(|s| ui::sanitize(s).into_owned())
        .or_else(|| me.get("key").and_then(name_of))
        .or_else(|| id_of(me))
        .unwrap_or_else(|| "API key".into())
}

pub fn auth(app: &mut App, cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::Login { with_token } => login(app, with_token),
        AuthCmd::Logout => {
            if auth::clear(&app.api_url)? {
                ui::success(&format!("Logged out of {}", app.api_url));
            } else {
                ui::info("No stored API key to remove.");
            }
            if std::env::var_os("FENDERS_API_KEY").is_some() {
                ui::warn(
                    "FENDERS_API_KEY is still set in your environment and will keep being used.",
                );
            }
            Ok(())
        }
        AuthCmd::Status => {
            let Some((token, source)) = auth::token(&app.api_url) else {
                bail!(ApiError {
                    status: 401,
                    code: "not_logged_in".into(),
                    message: format!("not logged in to {}", app.api_url),
                    request_id: None,
                });
            };
            let me = app.me()?;
            if app.out.json {
                app.out.print_json(&json!({
                    "api_url": app.api_url,
                    "source": source.to_string(),
                    "identity": me,
                }));
                return Ok(());
            }
            ui::success(&format!(
                "Logged in to {} as {}",
                ui::bold(&app.api_url, To::Err),
                ui::accent(&identity(&me), To::Err)
            ));
            ui::hint(&format!("Key {} from the {source}", mask(&token)));
            println!();
            app.out.record(&me);
            Ok(())
        }
        AuthCmd::Token => match auth::token(&app.api_url) {
            Some((t, _)) => {
                println!("{t}");
                Ok(())
            }
            None => bail!("not logged in — run `fenders auth login`"),
        },
    }
}

fn mask(t: &str) -> String {
    let head: String = t.chars().take(6).collect();
    format!("{head}…")
}

fn login(app: &mut App, with_token: bool) -> Result<()> {
    let key = if with_token {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        s.trim().to_string()
    } else {
        if !ui::interactive() {
            bail!(
                "no terminal to prompt on — pipe the key in with `--with-token`, or set FENDERS_API_KEY"
            );
        }
        eprint!("{}", ui::banner());
        eprintln!();
        ui::hint("Create an API key in the Fenders console: Settings → API keys.");
        crate::cmd::read_secret("API key", false)?
            .trim()
            .to_string()
    };
    if key.is_empty() {
        bail!("no API key given");
    }

    let client = Client::new(&app.api_url, Some(key.clone()), false)?;
    let pb = ui::spinner("Checking key");
    let me = client.get("/me", &[]);
    pb.finish_and_clear();
    let me = match me {
        Ok(m) => m,
        Err(e) => match e.downcast_ref::<ApiError>() {
            Some(api) if api.status == 401 => bail!("that API key was rejected by {}", app.api_url),
            _ => return Err(e),
        },
    };
    let source = auth::store(&app.api_url, &key)?;
    ui::success(&format!(
        "Logged in as {} {}",
        ui::accent(&identity(&me), To::Err),
        ui::dim(&format!("(key saved in the {source})"), To::Err)
    ));
    if std::env::var_os("FENDERS_API_KEY").is_some() {
        ui::warn("FENDERS_API_KEY is set and takes precedence over the saved key.");
    }

    app.client = client;
    if app.cfg.workspace.is_none() && ui::interactive() {
        choose(app, None)?;
    } else if app.cfg.workspace.is_none() {
        ui::hint("Next: pick a workspace and project with `fenders use`.");
    }
    Ok(())
}

pub fn use_(app: &mut App, args: UseArgs) -> Result<()> {
    choose(app, args.target.as_deref())
}

fn choose(app: &mut App, target: Option<&str>) -> Result<()> {
    let (ws_part, proj_part) = match target {
        Some(t) => match t.split_once('/') {
            Some((w, p)) => (Some(w), Some(p).filter(|p| !p.is_empty())),
            None => (Some(t), None),
        },
        None => (None, None),
    };
    let (ws, ws_name) = match ws_part {
        Some(w) => app.resolve_workspace(w)?,
        None if ui::interactive() => app.pick_workspace()?,
        None => bail!("give a target, e.g. `fenders use my-workspace/my-project`"),
    };
    let proj = match proj_part {
        Some(p) => Some(app.resolve_project(&ws, p)?),
        None if ui::interactive() => match app.pick_project(&ws) {
            Ok(p) => Some(p),
            Err(e) => {
                ui::warn(&e.to_string());
                None
            }
        },
        None => None,
    };
    app.cfg.set_workspace(ws.clone(), ws_name.clone());
    if let Some((id, name)) = &proj {
        app.cfg.set_project(id.clone(), name.clone());
    }
    app.cfg.save()?;
    let show = |name: &Option<String>, id: &str| name.clone().unwrap_or_else(|| id.to_string());
    let mut msg = format!(
        "Now using workspace {}",
        ui::accent(&show(&ws_name, &ws), To::Err)
    );
    if let Some((id, name)) = &proj {
        msg.push_str(&format!(
            " · project {}",
            ui::accent(&show(name, id), To::Err)
        ));
    }
    ui::success(&msg);
    Ok(())
}

pub fn context(app: &mut App) -> Result<()> {
    let source = auth::token(&app.api_url).map(|(_, s)| s.to_string());
    let ctx = json!({
        "api_url": app.api_url,
        "workspace": app.cfg.workspace,
        "workspace_name": app.cfg.workspace_name,
        "project": app.cfg.project,
        "project_name": app.cfg.project_name,
        "auth": source.clone().unwrap_or_else(|| "not logged in".into()),
        "config": crate::config::path().display().to_string(),
    });
    if app.out.json {
        app.out.print_json(&ctx);
        return Ok(());
    }
    let row = |k: &str, v: String| println!("  {}  {v}", ui::dim(&format!("{k:<10}"), To::Out));
    let named = |name: &Option<String>, id: &Option<String>| match (name, id) {
        (Some(n), Some(i)) => format!("{} {}", ui::bold(n, To::Out), ui::dim(i, To::Out)),
        (None, Some(i)) => i.clone(),
        _ => ui::dim("not set — run `fenders use`", To::Out),
    };
    row("api", app.api_url.clone());
    row(
        "auth",
        source.unwrap_or_else(|| ui::dim("not logged in — run `fenders auth login`", To::Out)),
    );
    row(
        "workspace",
        named(&app.cfg.workspace_name, &app.cfg.workspace),
    );
    row("project", named(&app.cfg.project_name, &app.cfg.project));
    row("config", crate::config::path().display().to_string());
    Ok(())
}
