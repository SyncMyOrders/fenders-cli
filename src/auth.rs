//! API key storage. `FENDERS_API_KEY` wins; otherwise the OS keychain on
//! macOS/Windows, falling back to a 0600 credentials file (always used on Linux,
//! which keeps the binary fully static).

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;

#[cfg(any(target_os = "macos", target_os = "windows"))]
const SERVICE: &str = "fenders-cli";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    Keychain,
    File,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Source::Env => "FENDERS_API_KEY environment variable",
            Source::Keychain => "system keychain",
            Source::File => "credentials file",
        })
    }
}

fn file() -> PathBuf {
    config::dir().join("credentials.toml")
}

fn read_file() -> BTreeMap<String, String> {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_file(map: &BTreeMap<String, String>) -> Result<()> {
    std::fs::create_dir_all(config::dir())?;
    let body = toml::to_string(map)?;
    let p = file();
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&p)
            .with_context(|| format!("cannot write {}", p.display()))?;
        f.write_all(body.as_bytes())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&p, body).with_context(|| format!("cannot write {}", p.display()))?;
    Ok(())
}

/// The key for `api_url`, and where it came from.
pub fn token(api_url: &str) -> Option<(String, Source)> {
    if let Ok(t) = std::env::var("FENDERS_API_KEY")
        && !t.trim().is_empty()
    {
        return Some((t.trim().to_string(), Source::Env));
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Ok(t) = keyring::Entry::new(SERVICE, api_url).and_then(|e| e.get_password()) {
        return Some((t, Source::Keychain));
    }
    read_file().remove(api_url).map(|t| (t, Source::File))
}

/// Store a key, preferring the keychain.
pub fn store(api_url: &str, key: &str) -> Result<Source> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        match keyring::Entry::new(SERVICE, api_url).and_then(|e| e.set_password(key)) {
            Ok(()) => {
                // Don't leave a stale plaintext copy behind.
                let mut m = read_file();
                if m.remove(api_url).is_some() {
                    write_file(&m)?;
                }
                return Ok(Source::Keychain);
            }
            Err(e) => crate::ui::warn(&format!(
                "keychain unavailable ({e}); storing the key in {}",
                file().display()
            )),
        }
    }
    let mut m = read_file();
    m.insert(api_url.to_string(), key.to_string());
    write_file(&m)?;
    Ok(Source::File)
}

/// Remove any stored key for `api_url`. Returns whether anything was removed.
pub fn clear(api_url: &str) -> Result<bool> {
    let mut removed = false;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Ok(e) = keyring::Entry::new(SERVICE, api_url) {
        removed |= e.delete_credential().is_ok();
    }
    let mut m = read_file();
    if m.remove(api_url).is_some() {
        write_file(&m)?;
        removed = true;
    }
    Ok(removed)
}
