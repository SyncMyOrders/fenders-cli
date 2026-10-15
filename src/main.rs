#[cfg(test)]
mod api_tests;
mod app;
mod auth;
mod client;
mod cmd;
mod config;
mod output;
mod ui;

use std::process::ExitCode;

use anyhow::Result;
use clap::builder::styling::{AnsiColor, Color, RgbColor, Style, Styles};
use clap::{Args, CommandFactory, Parser, Subcommand};

use crate::cmd::apps::{
    AppsCmd, EventsArgs, ReleasesCmd, RunArgs, RunsCmd, TriggersCmd, VersionsCmd,
};
use crate::cmd::deploy::DeployArgs;
use crate::cmd::logs::LogsArgs;
use crate::cmd::project::{
    ArtifactsCmd, ConnectionsCmd, HttpsCmd, OpsCmd, ProjectsCmd, ScheduleCmd, SecretsCmd,
    UsageArgs, WorkspacesCmd,
};
use crate::cmd::session::{AuthCmd, UseArgs};
use crate::output::Format;

fn styles() -> Styles {
    let orange = Style::new().fg_color(Some(Color::Rgb(RgbColor(
        ui::ORANGE.0,
        ui::ORANGE.1,
        ui::ORANGE.2,
    ))));
    Styles::styled()
        .header(orange.bold())
        .usage(orange.bold())
        .literal(Style::new().bold())
        .placeholder(Style::new().fg_color(Some(Color::Rgb(RgbColor(
            ui::MUTED.0,
            ui::MUTED.1,
            ui::MUTED.2,
        )))))
        .error(Style::new().fg_color(Some(AnsiColor::Red.into())).bold())
        .valid(Style::new().fg_color(Some(AnsiColor::Green.into())))
        .invalid(Style::new().fg_color(Some(AnsiColor::Yellow.into())))
}

const AFTER_HELP: &str = "\
Get started:
  fenders auth login              Store your workspace API key
  fenders use                     Pick a workspace and project
  fenders deploy app.wasm --http  Upload and deploy a WASI component
  fenders logs app -f             Follow its output

Docs: https://fenders.io";

#[derive(Parser, Debug)]
#[command(
    name = "fenders",
    version,
    about = "Deploy and run WebAssembly apps on Fenders — hardware-isolated microVM sandboxes.",
    styles = styles(),
    after_help = AFTER_HELP,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Args, Debug)]
struct Global {
    /// Workspace (name or id) for this command
    #[arg(
        short = 'w',
        long,
        global = true,
        env = "FENDERS_WORKSPACE",
        hide_env_values = true
    )]
    workspace: Option<String>,
    /// Project (name or id) for this command
    #[arg(
        short = 'p',
        long,
        global = true,
        env = "FENDERS_PROJECT",
        hide_env_values = true
    )]
    project: Option<String>,
    /// API base URL
    #[arg(
        long,
        global = true,
        env = "FENDERS_API_URL",
        hide_env_values = true,
        value_name = "URL"
    )]
    api_url: Option<String>,
    /// Output format
    #[arg(short = 'o', long, global = true, value_enum, default_value = "auto")]
    output: Format,
    /// Shorthand for --output json
    #[arg(long, global = true)]
    json: bool,
    /// Disable colours (also honours NO_COLOR)
    #[arg(long, global = true)]
    no_color: bool,
    /// Never prompt; fail instead
    #[arg(long, global = true)]
    no_input: bool,
    /// Log HTTP requests to stderr
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Log in, log out and inspect credentials
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Choose the workspace/project that commands apply to
    Use(UseArgs),
    /// Show the current API URL, workspace, project and auth
    Context,

    /// Upload and deploy a WASI component in one step
    Deploy(Box<DeployArgs>),
    /// Start a run of an app and wait for it
    Run(RunArgs),
    /// Show an app's output
    Logs(LogsArgs),

    /// Manage apps
    #[command(subcommand)]
    Apps(AppsCmd),
    /// Manage app versions
    #[command(subcommand)]
    Versions(VersionsCmd),
    /// Manage app releases
    #[command(subcommand)]
    Releases(ReleasesCmd),
    /// Inspect app runs
    #[command(subcommand)]
    Runs(RunsCmd),
    /// Show or change how an app is triggered
    #[command(subcommand)]
    Triggers(TriggersCmd),
    /// Show an app's events
    Events(EventsArgs),

    /// List workspaces
    #[command(subcommand)]
    Workspaces(WorkspacesCmd),
    /// List and create projects
    #[command(subcommand)]
    Projects(ProjectsCmd),
    /// Manage uploaded WASI components
    #[command(subcommand)]
    Artifacts(ArtifactsCmd),
    /// Manage external connections (HTTPS origins, TCP endpoints)
    #[command(subcommand)]
    Connections(ConnectionsCmd),
    /// Manage encrypted secrets
    #[command(subcommand)]
    Secrets(SecretsCmd),
    /// Inspect and wait for background operations
    #[command(subcommand)]
    Ops(OpsCmd),
    /// Show usage for the project or workspace
    Usage(UsageArgs),
    /// Show available app sizes and defaults
    Sizes,
    /// Show or validate the outbound HTTPS policy
    #[command(subcommand, name = "https-access")]
    HttpsAccess(HttpsCmd),
    /// Preview cron schedules
    #[command(subcommand)]
    Schedule(ScheduleCmd),

    /// Make an authenticated request to any API endpoint
    Api(cmd::api::ApiArgs),
    /// Update fenders to the latest release
    #[command(visible_alias = "upgrade")]
    Update(cmd::update::UpdateArgs),
    /// Print a shell completion script
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

fn main() -> ExitCode {
    // Exit quietly when stdout is closed early (`fenders apps list | head`)
    // instead of panicking on a broken pipe.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let g = &cli.global;
    ui::init(g.no_color, g.no_input);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report(&e);
            ExitCode::FAILURE
        }
    }
}

fn report(e: &anyhow::Error) {
    if let Some(api) = e.downcast_ref::<client::ApiError>() {
        ui::error(&api.to_string());
        if let Some(h) = api.hint() {
            ui::hint(&h);
        }
        if let Some(r) = &api.request_id {
            ui::hint(&format!("request id: {}", ui::sanitize(r)));
        }
        return;
    }
    ui::error(&ui::sanitize(&e.to_string()));
    for cause in e.chain().skip(1) {
        ui::hint(&format!("caused by: {}", ui::sanitize(&cause.to_string())));
    }
}

fn run(cli: Cli) -> Result<()> {
    let Some(command) = cli.cmd else {
        eprint!("{}", ui::banner());
        eprintln!();
        Cli::command().print_help()?;
        return Ok(());
    };
    if let Cmd::Completion { shell } = command {
        clap_complete::generate(
            shell,
            &mut Cli::command(),
            "fenders",
            &mut std::io::stdout(),
        );
        return Ok(());
    }

    if let Cmd::Update(a) = command {
        let json = cli.global.json || cli.global.output == Format::Json;
        return cmd::update::run(a, json);
    }

    let g = cli.global;
    let cfg = config::Config::load()?;
    let api_url = g
        .api_url
        .clone()
        .or_else(|| cfg.api_url.clone())
        .unwrap_or_else(|| config::DEFAULT_API_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    let token = auth::token(&api_url).map(|(t, _)| t);
    let client = client::Client::new(&api_url, token, g.verbose)?;
    let format = if g.json { Format::Json } else { g.output };
    let mut app = app::App::new(
        client,
        output::Out::new(format),
        cfg,
        api_url,
        g.workspace,
        g.project,
    );

    use cmd::*;
    match command {
        Cmd::Auth(c) => session::auth(&mut app, c),
        Cmd::Use(a) => session::use_(&mut app, a),
        Cmd::Context => session::context(&mut app),
        Cmd::Deploy(a) => deploy::run(&mut app, *a),
        Cmd::Run(a) => apps::run(&mut app, a),
        Cmd::Logs(a) => logs::run(&mut app, a),
        Cmd::Apps(c) => apps::apps(&mut app, c),
        Cmd::Versions(c) => apps::versions(&mut app, c),
        Cmd::Releases(c) => apps::releases(&mut app, c),
        Cmd::Runs(c) => apps::runs(&mut app, c),
        Cmd::Triggers(c) => apps::triggers(&mut app, c),
        Cmd::Events(a) => apps::events(&mut app, a),
        Cmd::Workspaces(c) => project::workspaces(&mut app, c),
        Cmd::Projects(c) => project::projects(&mut app, c),
        Cmd::Artifacts(c) => project::artifacts(&mut app, c),
        Cmd::Connections(c) => project::connections(&mut app, c),
        Cmd::Secrets(c) => project::secrets(&mut app, c),
        Cmd::Ops(c) => project::ops(&mut app, c),
        Cmd::Usage(a) => project::usage(&mut app, a),
        Cmd::Sizes => project::sizes(&mut app),
        Cmd::HttpsAccess(c) => project::https(&mut app, c),
        Cmd::Schedule(c) => project::schedule(&mut app, c),
        Cmd::Api(a) => api::run(&mut app, a),
        Cmd::Completion { .. } | Cmd::Update(_) => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_is_well_formed() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }
}
