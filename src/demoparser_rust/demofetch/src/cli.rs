use crate::{input_matches, Callback, Cancel, FaceitClient};
use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Download,
    Trim,
    Parse,
}

#[derive(Debug, Parser)]
#[command(about = "Download FACEIT demos locally; optionally trim and parse with Demoparser")]
pub struct Args {
    /// Match UUIDs or FACEIT match-room URLs (multiple accepted).
    pub matches: Vec<String>,
    /// ACE text queue (ace*.txt) with one match ID/URL per line.
    #[arg(long, value_parser = ace_queue_path)]
    pub matches_file: Option<PathBuf>,
    /// Read only ACE text queues (ace*.txt) in this directory and its subdirectories.
    #[arg(long)]
    pub matches_directory: Option<PathBuf>,
    /// Add a player's most recent CS2 matches.
    #[arg(long)]
    pub nickname: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=10000))]
    pub limit: u32,
    /// Hone in on the oldest downloadable demos and queue them first.
    #[arg(long)]
    pub oldest_available: bool,
    /// Search this many days of player history rather than the newest --limit matches.
    #[arg(long, default_value_t = 365, value_parser = clap::value_parser!(u32).range(1..=3650))]
    pub search_days: u32,
    /// Maximum availability checks; older unchecked candidates remain explicit in the report.
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(2..=10000))]
    pub probe_budget: u32,
    /// Remove confirmed-unavailable entries from --matches-file after saving the search report.
    #[arg(long, requires = "oldest_available")]
    pub purge_expired: bool,
    /// Save the availability report and oldest-first queue without transferring full demos.
    #[arg(long, requires = "oldest_available")]
    pub discover_only: bool,
    /// Destination drive/folder. Demos stay compressed until the parser needs them.
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Store reports independently of demos; defaults to Anomalous local app storage.
    #[arg(long)]
    pub report_directory: Option<PathBuf>,
    /// download: save only; trim: catalog + round clips; parse: clips + tick/replay data.
    #[arg(long, value_enum, default_value_t = Mode::Download)]
    pub mode: Mode,
    /// Concurrent downloads (INI default, otherwise 3).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=16))]
    pub concurrency: Option<u8>,
    /// Credential INI path; defaults to faceit.ini beside this executable.
    #[arg(long)]
    pub credentials: Option<PathBuf>,
    /// Override parser config.ini for trim/parse modes.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Maximum ready demo files passed to each serial parser job.
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=64))]
    pub parse_batch_size: u8,
    /// Download progress as JSON lines. Parser progress uses Demoparser's existing NDJSON.
    #[arg(long)]
    pub ndjson: bool,
}

pub fn is_ace_queue(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    name.ends_with(".txt")
        && (name == "ace.txt" || name.starts_with("ace_") || name.starts_with("ace-"))
}

fn ace_queue_path(value: &str) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !is_ace_queue(&path) {
        return Err("Use an ACE queue named ace.txt, ace_*.txt, or ace-*.txt; QUAD and general match lists are not download sources".into());
    }
    Ok(path)
}

impl Args {
    pub async fn resolve_matches(
        &self,
        client: &FaceitClient,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<Vec<String>> {
        let mut inputs = self.matches.join("\n");
        let mut queue_files = Vec::new();
        if let Some(path) = &self.matches_file {
            if !is_ace_queue(path) {
                bail!("Only ACE text queues are accepted as download sources: {}", path.display());
            }
            queue_files.push(path.clone());
        }
        if let Some(directory) = &self.matches_directory {
            queue_files.extend(queue_files_in(directory)?);
        }
        queue_files.sort();
        queue_files.dedup();
        if self.purge_expired && queue_files.is_empty() {
            bail!("--purge-expired needs --matches-file or --matches-directory");
        }
        for path in &queue_files {
            crate::check_cancel(cancel)?;
            inputs.push('\n');
            inputs.push_str(&tokio::fs::read_to_string(path).await?);
        }
        let entries = input_matches(&inputs)?;
        let mut ids: Vec<_> = entries.iter().map(|entry| entry.match_id.clone()).collect();
        if self.oldest_available {
            let settings = crate::settings::Settings::load(self.credentials.as_deref())?;
            let output = self
                .output
                .clone()
                .or(settings.output_directory)
                .unwrap_or_else(|| "D:/".into());
            let undated: Vec<_> = entries
                .iter()
                .filter(|entry| entry.finished_at.is_none())
                .map(|entry| entry.match_id.clone())
                .collect();
            let mut candidates = client.dated_matches(&undated, cancel, callback).await?;
            candidates.extend(entries.iter().filter_map(|entry| {
                entry
                    .finished_at
                    .map(|finished_at| crate::availability::Candidate {
                        match_id: entry.match_id.clone(),
                        finished_at,
                        date_is_hint: entry.date_is_hint,
                    })
            }));
            if let Some(nickname) = &self.nickname {
                candidates.extend(
                    client
                        .availability_history(nickname, self.search_days, cancel, callback)
                        .await?,
                );
            }
            if candidates.is_empty() {
                bail!("No completed matches in the requested search scope");
            }
            let scope = format!(
                "Supplied matches and player {:?}, within {} days when fetching history",
                self.nickname, self.search_days
            );
            let report = client
                .find_oldest_available(
                    candidates,
                    &output,
                    self.limit as usize,
                    self.probe_budget as usize,
                    scope,
                    cancel,
                    callback,
                )
                .await?;
            let report_directory = self.report_directory.clone().unwrap_or_else(crate::default_report_directory);
            std::fs::create_dir_all(&report_directory)?;
            // Save evidence before removing any dead link from the original queue.
            let run_stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            crate::save_report(
                &report_directory.join(format!("faceit-availability-{run_stamp}.json")),
                &report,
            )?;
            crate::save_report(&report_directory.join("faceit-availability.json"), &report)?;
            if self.purge_expired {
                for path in &queue_files {
                    crate::availability::purge_queue(path, &report.unavailable)?;
                }
            }
            return Ok(report.queue);
        }
        if let Some(nickname) = &self.nickname {
            ids.extend(
                client
                    .player_matches(nickname, self.limit as usize, cancel, callback)
                    .await?,
            );
        }
        let mut seen = HashSet::new();
        ids.retain(|id| seen.insert(id.clone()));
        if ids.is_empty() {
            bail!("Provide match IDs, --matches-file, --matches-directory, or --nickname");
        }
        Ok(ids)
    }
}

pub fn queue_files_in(directory: &std::path::Path) -> Result<Vec<PathBuf>> {
    if !directory.is_dir() {
        bail!(
            "Match queue directory does not exist: {}",
            directory.display()
        );
    }
    let mut pending = vec![directory.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            // Do not recurse through symlinks/junctions into unrelated folders.
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && is_ace_queue(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    if files.is_empty() {
        bail!("No ACE text queues found in {} (expected ace.txt, ace_*.txt, or ace-*.txt)", directory.display());
    }
    Ok(files)
}
