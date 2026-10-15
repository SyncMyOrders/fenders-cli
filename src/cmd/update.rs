//! `fenders update` — check for and install a newer release via install.sh,
//! replacing the binary that is currently running.
//!
//! The installer is taken from the target release itself and must match that
//! release's SHA256SUMS before it runs. It is piped to `sh` on stdin, so no
//! script is ever written to a shared temp directory.

use std::io::Write;
use std::process::{Command, Stdio};
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

/// `redirects`: release downloads redirect to GitHub's CDN; the
/// `/releases/latest` lookup must not follow its redirect.
fn http(redirects: bool) -> Result<reqwest::blocking::Client> {
    let policy = if redirects {
        reqwest::redirect::Policy::limited(5)
    } else {
        reqwest::redirect::Policy::none()
    };
    let mut b = reqwest::blocking::Client::builder()
        .user_agent(concat!("fenders-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .redirect(policy);
    // Only an explicitly configured mirror may use plain HTTP.
    if std::env::var_os("FENDERS_RELEASES_URL").is_none() {
        b = b.https_only(true);
    }
    Ok(b.build()?)
}

fn download(url: &str) -> Result<Vec<u8>> {
    Ok(http(true)?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("could not download {url}"))?
        .bytes()?
        .to_vec())
}

/// The expected SHA-256 for `file` from a SHA256SUMS listing.
fn expected_sum<'a>(sums: &'a str, file: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let (hash, name) = (parts.next()?, parts.next()?);
        (name.trim_start_matches('*') == file).then_some(hash)
    })
}

/// Download install.sh from the `tag` release and verify it against that
/// release's SHA256SUMS.
fn verified_installer(tag: &str) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let base = format!("{}/download/{tag}", releases_url());
    let sums = String::from_utf8(download(&format!("{base}/SHA256SUMS"))?)
        .context("SHA256SUMS is not text")?;
    let want = expected_sum(&sums, "install.sh")
        .with_context(|| format!("release {tag} does not list install.sh in SHA256SUMS"))?
        .to_ascii_lowercase();
    let script = download(&format!("{base}/install.sh"))?;
    let got: String = Sha256::digest(&script)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != want {
        bail!("install.sh from release {tag} failed checksum verification");
    }
    Ok(script)
}

/// The latest release tag, read from the `/releases/latest` redirect (no API
/// rate limits).
fn latest_tag() -> Result<String> {
    let resp = http(false)?
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

    // Run the verified installer from this release against our own directory.
    let script = verified_installer(&target)?;
    let mut child = Command::new("sh")
        .args(["-s", "--", "--version", &target, "--dir"])
        .arg(dir)
        .stdin(Stdio::piped())
        .spawn()
        .context("could not run sh")?;
    child
        .stdin
        .take()
        .context("no stdin for sh")?
        .write_all(&script)?;
    let status = child.wait()?;
    if !status.success() {
        bail!("the installer failed (exit status {status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{expected_sum, parse};

    #[test]
    fn reads_checksums() {
        let sums = "abc123  fenders-v1-x.tar.gz\ndef456 *install.sh\n";
        assert_eq!(expected_sum(sums, "install.sh"), Some("def456"));
        assert_eq!(expected_sum(sums, "fenders-v1-x.tar.gz"), Some("abc123"));
        assert_eq!(expected_sum(sums, "missing"), None);
    }

    #[test]
    fn compares_versions() {
        assert!(parse("v0.10.0") > parse("0.9.3"));
        assert!(parse("v1.0.0") > parse("v0.99.99"));
        assert_eq!(parse("v1.2"), Some((1, 2, 0)));
        assert_eq!(parse("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse("garbage"), None);
    }
}
