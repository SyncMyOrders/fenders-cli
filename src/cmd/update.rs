//! `fenders update` — check for and install a newer release via install.sh,
//! replacing the binary that is currently running.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::json;

use crate::ui::{self, To};

const REPO: &str = "SyncMyOrders/fenders-cli";

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Only report whether an update is available
    #[arg(long)]
    pub check: bool,
    /// Install this release (e.g. v0.2.0) instead of the latest
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
}

fn releases_url() -> String {
    std::env::var("FENDERS_RELEASES_URL")
        .unwrap_or_else(|_| format!("https://github.com/{REPO}/releases"))
}

fn script_url() -> String {
    std::env::var("FENDERS_INSTALL_SCRIPT_URL")
        .unwrap_or_else(|_| format!("https://raw.githubusercontent.com/{REPO}/main/install.sh"))
}

fn http() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("fenders-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// The latest release tag, read from the `/releases/latest` redirect (no API
/// rate limits).
fn latest_tag() -> Result<String> {
    let resp = http()?
        .get(format!("{}/latest", releases_url()))
        .send()
        .context("could not reach GitHub to check for updates")?;
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    match location.rsplit_once("/tag/") {
        Some((_, tag)) if !tag.is_empty() => Ok(tag.to_string()),
        _ => bail!("no published release found at {}", releases_url()),
    }
}

/// Parse "v1.2.3" / "1.2.3" into a comparable tuple.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        it.next()??,
        it.next().flatten().unwrap_or(0),
        it.next().flatten().unwrap_or(0),
    ))
}

pub fn run(args: UpdateArgs, json_out: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let target = match &args.version {
        Some(v) => format!("v{}", v.trim_start_matches('v')),
        None => latest_tag()?,
    };
    let newer = match (parse(&target), parse(current)) {
        (Some(t), Some(c)) => t > c,
        _ => target.trim_start_matches('v') != current,
    };

    if args.check {
        if json_out {
            println!(
                "{}",
                json!({ "current": current, "latest": target, "update_available": newer })
            );
        } else if newer {
            ui::info(&format!(
                "fenders {} is available (you have {current}). Run {} to install it.",
                ui::accent(&target, To::Err),
                ui::bold("fenders update", To::Err)
            ));
        } else {
            ui::success(&format!("fenders {current} is up to date"));
        }
        return Ok(());
    }
    if args.version.is_none() && !newer {
        ui::success(&format!("fenders {current} is up to date"));
        return Ok(());
    }
    if cfg!(windows) {
        bail!(
            "self-update isn't supported on Windows yet — download {target} from {}",
            releases_url()
        );
    }

    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("cannot locate the running fenders binary")?;
    let dir = exe.parent().context("binary has no parent directory")?;
    if dir.components().any(|c| c.as_os_str() == ".cargo") {
        ui::warn(
            "This fenders was installed with cargo; `cargo install` would be the usual way to update it.",
        );
    }

    // Fetch the installer and run it against our own directory.
    let script = http()?
        .get(script_url())
        .send()
        .and_then(|r| r.error_for_status())
        .context("could not download the installer")?
        .text()?;
    if !script.starts_with("#!/bin/sh") {
        bail!("unexpected installer contents from {}", script_url());
    }
    let tmp = std::env::temp_dir().join(format!("fenders-install-{}.sh", std::process::id()));
    std::fs::write(&tmp, script)?;
    let status = std::process::Command::new("sh")
        .arg(&tmp)
        .arg("--dir")
        .arg(dir)
        .arg("--version")
        .arg(&target)
        .status();
    let _ = std::fs::remove_file(&tmp);
    let status = status.context("could not run sh")?;
    if !status.success() {
        bail!("the installer failed (exit status {status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn compares_versions() {
        assert!(parse("v0.10.0") > parse("0.9.3"));
        assert!(parse("v1.0.0") > parse("v0.99.99"));
        assert_eq!(parse("v1.2"), Some((1, 2, 0)));
        assert_eq!(parse("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse("garbage"), None);
    }
}
