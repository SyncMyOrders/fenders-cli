//! Persistent CLI configuration: API URL and the current workspace/project.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_API_URL: &str = "https://platform.fenders.io/v1";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
}

/// `$FENDERS_CONFIG_DIR`, else `~/.config/fenders` (Unix) or `%APPDATA%\fenders`.
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("FENDERS_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("fenders")
    }
    #[cfg(not(windows))]
    {
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(x).join("fenders");
        }
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join("fenders")
    }
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self> {
        let p = path();
        match std::fs::read_to_string(&p) {
            Ok(s) => {
                toml::from_str(&s).with_context(|| format!("invalid config file {}", p.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", p.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(dir()).context("cannot create config directory")?;
        let s = toml::to_string_pretty(self)?;
        std::fs::write(path(), s).with_context(|| format!("cannot write {}", path().display()))
    }

    pub fn set_workspace(&mut self, id: String, name: Option<String>) {
        if self.workspace.as_deref() != Some(id.as_str()) {
            self.project = None;
            self.project_name = None;
        }
        self.workspace = Some(id);
        self.workspace_name = name;
    }

    pub fn set_project(&mut self, id: String, name: Option<String>) {
        self.project = Some(id);
        self.project_name = name;
    }
}
