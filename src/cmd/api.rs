//! `fenders api` — authenticated raw requests to any endpoint.

use std::io::Read;

use anyhow::{Context, Result, bail};
use clap::Args;
use reqwest::Method;
use serde_json::Value;

use crate::app::App;
use crate::client::Body;

#[derive(Args, Debug)]
pub struct ApiArgs {
    /// Path relative to the API base, e.g. /me or /workspaces/{workspace}/projects.
    /// {workspace} and {project} are filled from the current context.
    pub path: String,
    /// HTTP method
    #[arg(short = 'X', long, default_value = "GET")]
    pub method: String,
    /// JSON body: inline, @file, or @- for stdin
    #[arg(short, long, value_name = "JSON|@FILE")]
    pub data: Option<String>,
    /// Query parameter, repeatable
    #[arg(short, long = "query", value_name = "KEY=VALUE")]
    pub query: Vec<String>,
}

pub fn run(app: &mut App, args: ApiArgs) -> Result<()> {
    let mut path = args.path.clone();
    if path.contains("{workspace}") {
        path = path.replace("{workspace}", &app.workspace()?);
    }
    if path.contains("{project}") {
        path = path.replace("{project}", &app.project()?);
    }
    let method =
        Method::from_bytes(args.method.to_uppercase().as_bytes()).context("invalid HTTP method")?;
    let body = match args.data {
        None => Body::None,
        Some(d) => {
            let raw = match d.strip_prefix('@') {
                Some("-") => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
                Some(file) => {
                    std::fs::read_to_string(file).with_context(|| format!("cannot read {file}"))?
                }
                None => d,
            };
            Body::Json(serde_json::from_str::<Value>(&raw).context("--data is not valid JSON")?)
        }
    };
    let mut query = vec![];
    for q in &args.query {
        match q.split_once('=') {
            Some((k, v)) => query.push((k.to_string(), v.to_string())),
            None => bail!("--query must look like KEY=VALUE"),
        }
    }
    let res = app.client.send(method, &path, &query, body)?;
    if !res.is_null() {
        app.out.print_json(&res);
    }
    Ok(())
}
