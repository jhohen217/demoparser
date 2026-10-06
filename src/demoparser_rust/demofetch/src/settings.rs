//! Portable credentials and defaults alongside Demoparser.exe.
use crate::Credentials;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub credentials: Credentials,
    pub output_directory: Option<PathBuf>,
    pub concurrency: Option<usize>,
}

impl Settings {
    /// Prefer faceit.ini beside the executable, then the working directory. Also accept a FACEIT
    /// section in config.ini. An explicit path is never silently replaced by an auto-detected one.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let discovered = if let Some(path) = path {
            Some(path.to_owned())
        } else {
            let exe_dir = std::env::current_exe()?
                .parent()
                .context("Executable has no parent directory")?
                .to_owned();
            [
                exe_dir.join("faceit.ini"),
                PathBuf::from("faceit.ini"),
                exe_dir.join("config.ini"),
                PathBuf::from("config.ini"),
            ]
            .into_iter()
            .find(|path| path.is_file())
        };
        let mut settings = if let Some(path) = discovered {
            Self::parse(
                &std::fs::read_to_string(&path)
                    .with_context(|| format!("Could not read {}", path.display()))?,
            )?
        } else {
            Self::default()
        };
        if let Ok(value) = std::env::var("FACEIT_API_KEY") {
            if !value.trim().is_empty() {
                settings.credentials.data_key = value;
            }
        }
        if let Ok(value) = std::env::var("FACEIT_DOWNLOAD_TOKEN") {
            if !value.trim().is_empty() {
                settings.credentials.download_token = value;
            }
        }
        Ok(settings)
    }

    pub fn parse(contents: &str) -> Result<Self> {
        let mut settings = Self::default();
        let mut section = String::new();
        for line in contents.trim_start_matches('\u{feff}').lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].trim().to_ascii_lowercase();
                continue;
            }
            if section != "faceit" && section != "keys" {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bail!("Invalid FACEIT INI setting (expected key=value)");
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            match (section.as_str(), key.as_str()) {
                ("faceit", "dataapikey" | "apikey") | ("keys", "faceit_api_key") => {
                    settings.credentials.data_key = value.into()
                }
                ("faceit", "downloadtoken") | ("keys", "faceit_download_token") => {
                    settings.credentials.download_token = value.into()
                }
                ("faceit", "downloaddirectory") if !value.is_empty() => {
                    settings.output_directory = Some(value.into())
                }
                ("faceit", "concurrency") if !value.is_empty() => {
                    let concurrency: usize = value.parse().map_err(|_| {
                        anyhow::anyhow!("FACEIT Concurrency must be a number from 1 to 16")
                    })?;
                    if !(1..=16).contains(&concurrency) {
                        bail!("FACEIT Concurrency must be between 1 and 16");
                    }
                    settings.concurrency = Some(concurrency);
                }
                _ => {}
            }
        }
        Ok(settings)
    }
}
