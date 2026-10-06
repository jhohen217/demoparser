//! Shared CLI/GUI acquisition workflow. Downloads overlap parsing, but catalog writes are serial.

use crate::{run_parsing, InputKind, ProgressEvent};
use anyhow::{bail, Result};
use config::AppConfig;
use demofetch::cli::Mode;
use demofetch::{Callback, Cancel, FaceitClient, MatchResult};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{atomic::Ordering, Arc};

#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub matches: Vec<String>,
    pub output: PathBuf,
    pub report_directory: Option<PathBuf>,
    pub concurrency: usize,
    pub parse_batch_size: usize,
    pub mode: Mode,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessingResult {
    pub files: Vec<PathBuf>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FetchReport {
    pub report_path: PathBuf,
    pub downloads: Vec<MatchResult>,
    pub processing: Vec<ProcessingResult>,
    pub canceled: bool,
}

impl FetchReport {
    pub fn has_errors(&self) -> bool {
        self.downloads.iter().any(|result| result.error.is_some())
            || self
                .processing
                .iter()
                .any(|result| !result.errors.is_empty())
    }
}

pub async fn run_fetch<F>(
    request: FetchRequest,
    client: FaceitClient,
    config: Option<AppConfig>,
    cancel: Cancel,
    download_callback: Callback,
    parser_callback: F,
) -> Result<FetchReport>
where
    F: Fn(ProgressEvent) + Send + Sync + 'static,
{
    if request.matches.is_empty() {
        bail!("No matches to download");
    }
    if !(1..=16).contains(&request.concurrency) {
        bail!("Download concurrency must be between 1 and 16");
    }
    if !(1..=64).contains(&request.parse_batch_size) {
        bail!("Parser batch size must be between 1 and 64");
    }
    let mut config = if request.mode == Mode::Download {
        None
    } else {
        let mut config = config
            .ok_or_else(|| anyhow::anyhow!("Trim/parse mode requires parser configuration"))?;
        config.parser.trim_collection_rounds = true;
        config.parser.process_tick_data = request.mode == Mode::Parse;
        // Honor the host's explicit cleanup setting. The integrated trimmer only deletes after
        // every requested clip is verified and the catalog commit succeeds.
        Some(config)
    };
    std::fs::create_dir_all(&request.output)?;
    let report_directory = request.report_directory.unwrap_or_else(demofetch::default_report_directory);
    std::fs::create_dir_all(&report_directory)?;
    let report_path = report_directory.join(format!(
        "faceit-run-{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.9f")
    ));
    let mut report = FetchReport {
        report_path: report_path.clone(),
        ..FetchReport::default()
    };
    demofetch::save_report(&report_path, &report)?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(request.concurrency);
    let producer_client = client;
    let downloads = demofetch::download_batch(
        producer_client,
        request.matches,
        request.output,
        request.concurrency,
        cancel.clone(),
        download_callback,
        sender,
    );
    let parser_callback = Arc::new(parser_callback);
    // Join instead of detaching the downloader: any consumer error drops its receiver and all
    // worker futures, so failed report writes cannot leave downloads running in the background.
    let consumer = async {
        let mut ready_files = std::collections::VecDeque::new();
        let mut channel_closed = false;
        loop {
            if ready_files.is_empty() && !channel_closed {
                if let Some(result) = receiver.recv().await {
                    ready_files.extend(result.files.iter().map(|file| file.path.clone()));
                    report.downloads.push(result);
                    demofetch::save_report(&report_path, &report)?;
                } else {
                    channel_closed = true;
                }
            }
            while let Ok(result) = receiver.try_recv() {
                ready_files.extend(result.files.iter().map(|file| file.path.clone()));
                report.downloads.push(result);
                demofetch::save_report(&report_path, &report)?;
            }
            if cancel.load(Ordering::Relaxed) {
                ready_files.clear();
            }
            if config.is_none() {
                ready_files.clear();
            }
            if !ready_files.is_empty() {
                let files: Vec<_> = ready_files
                    .drain(..ready_files.len().min(request.parse_batch_size))
                    .collect();
                let callback = parser_callback.clone();
                let outcome = run_parsing(
                    files.clone(),
                    config.as_mut().unwrap().clone(),
                    move |event| callback(event),
                    None,
                    false,
                    InputKind::All,
                    Some(cancel.clone()),
                )
                .await;
                let errors = match outcome {
                    Ok((batch, _)) => batch
                        .errors
                        .into_iter()
                        .map(|(path, error)| format!("{}: {error}", path.display()))
                        .collect(),
                    Err(error) => vec![format!("{error:#}")],
                };
                report.processing.push(ProcessingResult { files, errors });
                demofetch::save_report(&report_path, &report)?;
            }
            if channel_closed && ready_files.is_empty() {
                break;
            }
        }
        report.canceled = cancel.load(Ordering::Relaxed);
        demofetch::save_report(&report_path, &report)?;
        Ok::<_, anyhow::Error>(report)
    };
    let (_, report) = tokio::try_join!(downloads, consumer)?;
    Ok(report)
}
