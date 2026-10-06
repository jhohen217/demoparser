//! FACEIT Data API -> Downloads API -> streamed, validated demo archives.
//! Credentials are used only for their respective API, never for the signed CDN URL.

pub mod availability;
pub mod cli;
mod rate_limit;
pub mod settings;

/// Downloader bookkeeping belongs to app storage, independently of the demo drive.
pub fn default_report_directory() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|root| std::path::PathBuf::from(root).join("ANOMALOUS/jobs/FACEIT"))
        .unwrap_or_else(|| std::env::current_exe().ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_else(|| std::path::PathBuf::from(".")).join("logs/FACEIT"))
}

use anyhow::{bail, Context, Result};
use reqwest::{Client, Method, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(Clone, Default)]
pub struct Credentials {
    pub data_key: String,
    pub download_token: String,
}

impl Credentials {
    pub fn from_env() -> Result<Self> {
        let credentials = Self {
            data_key: std::env::var("FACEIT_API_KEY").unwrap_or_default(),
            download_token: std::env::var("FACEIT_DOWNLOAD_TOKEN").unwrap_or_default(),
        };
        credentials.validate()?;
        Ok(credentials)
    }

    pub fn validate(&self) -> Result<()> {
        if self.data_key.trim().is_empty() {
            bail!(
                "Set [FACEIT] DataApiKey in faceit.ini beside the executable (or FACEIT_API_KEY)"
            );
        }
        if self.download_token.trim().is_empty() {
            bail!("Set [FACEIT] DownloadToken in faceit.ini beside the executable (or FACEIT_DOWNLOAD_TOKEN); this token needs Downloads API access");
        }
        Ok(())
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { [redacted] }")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedDemo {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    pub match_id: String,
    pub files: Vec<DownloadedDemo>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    AvailabilityProbe {
        match_id: String,
        finished_at: u64,
        outcome: availability::Availability,
        probes: usize,
        budget: usize,
    },
    AvailabilitySearchFinished {
        report: availability::SearchReport,
    },
    Started {
        matches: usize,
    },
    Downloading {
        match_id: String,
        demo: usize,
        total_demos: usize,
    },
    DownloadProgress {
        match_id: String,
        bytes: u64,
        total_bytes: Option<u64>,
    },
    MatchCompleted {
        result: MatchResult,
    },
    Retrying {
        operation: String,
        attempt: usize,
        wait_seconds: u64,
    },
}

pub type Callback = Arc<dyn Fn(Event) + Send + Sync>;
pub type Cancel = Arc<AtomicBool>;

#[derive(Debug)]
struct ApiStatus {
    operation: String,
    status: StatusCode,
}
impl std::fmt::Display for ApiStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: HTTP {}", self.operation, self.status)?;
        if self.status == StatusCode::UNAUTHORIZED || self.status == StatusCode::FORBIDDEN {
            write!(f, "; check the credential and API permission. Downloads API access is granted separately by FACEIT.")?;
        }
        Ok(())
    }
}
impl std::error::Error for ApiStatus {}

pub fn check_cancel(cancel: &Cancel) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Canceled");
    }
    Ok(())
}

async fn cancel_wait(cancel: &Cancel) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Accept UUIDs, FACEIT `1-UUID` identifiers and match-room links. Normalization also deduplicates
/// the legacy Python queue's prefixed and unprefixed forms.
pub fn match_id(input: &str) -> Result<String> {
    let input = input.trim().trim_start_matches('\u{feff}');
    let id = if input.starts_with("https://") || input.starts_with("http://") {
        let url = Url::parse(input).context("Invalid match-room URL")?;
        let host = url.host_str().unwrap_or_default();
        if host != "faceit.com" && !host.ends_with(".faceit.com") {
            bail!("Match-room URL must belong to faceit.com");
        }
        let parts: Vec<_> = url
            .path_segments()
            .context("Invalid match-room URL")?
            .collect();
        let index = parts
            .iter()
            .position(|part| *part == "room")
            .context("Expected a FACEIT /room/<match-id> URL")?;
        parts
            .get(index + 1)
            .context("Missing match ID in room URL")?
            .to_string()
    } else {
        legacy_prefix(input)
            .map(|(_, id)| id)
            .unwrap_or(input)
            .to_string()
    };
    let uuid = uuid::Uuid::parse_str(id.strip_prefix("1-").unwrap_or(&id))
        .context("Expected a match UUID, 1-UUID, or FACEIT match-room URL")?;
    Ok(format!("1-{uuid}"))
}

pub fn match_inputs(input: &str) -> Result<Vec<String>> {
    Ok(input_matches(input)?
        .into_iter()
        .map(|entry| entry.match_id)
        .collect())
}

fn legacy_prefix(input: &str) -> Option<(&str, &str)> {
    let mut parts = input.splitn(3, '_');
    let date = parts.next()?;
    let counts = parts.next()?;
    let id = parts.next()?;
    if date.len() != 8 || counts.len() != 4 || !counts.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if date != "00-00-00" {
        chrono::NaiveDate::parse_from_str(date, "%m-%d-%y").ok()?;
    }
    Some((date, id))
}

#[derive(Debug, Clone)]
pub struct InputMatch {
    pub match_id: String,
    pub finished_at: Option<u64>,
    pub date_is_hint: bool,
}

fn queue_timestamp(input: &str) -> Option<u64> {
    let seconds = chrono::DateTime::parse_from_rfc3339(input)
        .map(|date| date.timestamp())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|date| date.and_utc().timestamp())
        })
        .ok()?;
    u64::try_from(seconds).ok()
}

/// Legacy dates are ordering hints; the four digits after the date are ACE/QUAD counts.
pub fn input_matches(input: &str) -> Result<Vec<InputMatch>> {
    let mut entries: Vec<InputMatch> = Vec::new();
    let mut indices = std::collections::HashMap::new();
    for line in input.lines() {
        let line = line
            .trim_start_matches('\u{feff}')
            .split('#')
            .next()
            .unwrap_or_default();
        let mut items = line
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|v| !v.is_empty())
            .peekable();
        while let Some(item) = items.next() {
            let id = match_id(item).with_context(|| format!("Invalid match entry: {item}"))?;
            let date = legacy_prefix(item)
                .and_then(|(date, _)| chrono::NaiveDate::parse_from_str(date, "%m-%d-%y").ok())
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .and_then(|date| u64::try_from(date.and_utc().timestamp()).ok());
            let exact = items.peek().and_then(|item| queue_timestamp(item));
            if exact.is_some() {
                items.next();
            }
            let entry = InputMatch {
                match_id: id.clone(),
                finished_at: exact.or(date),
                date_is_hint: exact.is_none() && date.is_some(),
            };
            if let Some(&index) = indices.get(&id) {
                let previous: &mut InputMatch = &mut entries[index];
                if entry.finished_at.is_some()
                    && (previous.finished_at.is_none()
                        || (previous.date_is_hint && !entry.date_is_hint))
                {
                    *previous = entry;
                }
            } else {
                indices.insert(id, entries.len());
                entries.push(entry);
            }
        }
    }
    Ok(entries)
}

fn demo_path(
    output: &Path,
    id: &str,
    metadata: &serde_json::Value,
    index: usize,
    format: DemoFormat,
) -> Result<PathBuf> {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let specified_month = output
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            MONTHS.iter().any(|month| {
                name.strip_prefix(month)
                    .is_some_and(|year| year.len() == 2 && year.bytes().all(|b| b.is_ascii_digit()))
            })
        });
    let directory = if specified_month {
        output.to_path_buf()
    } else {
        let date = metadata["finished_at"]
            .as_i64()
            .filter(|date| *date > 0)
            .or_else(|| metadata["started_at"].as_i64().filter(|date| *date > 0))
            .and_then(|date| chrono::DateTime::from_timestamp(date, 0))
            .context("Match has no usable date for the MonthYY destination folder")?;
        output.join(date.format("%B%y").to_string())
    };
    let stem = if index == 0 {
        id.to_owned()
    } else {
        format!("{id}-map{}", index + 1)
    };
    Ok(directory.join(format!("{stem}.{}", format.extension())))
}

#[derive(Clone)]
pub struct FaceitClient {
    api: Client,
    cdn: Client,
    credentials: Credentials,
    data_base: String,
    download_endpoint: String,
    retries: usize,
    rate: Arc<rate_limit::RateGate>,
    cdn_rate: Arc<rate_limit::RateGate>,
    monthly_layout: bool,
}

impl FaceitClient {
    pub fn new(credentials: Credentials) -> Result<Self> {
        credentials.validate()?;
        Ok(Self {
            api: Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("demofetch/0.1")
                .build()?,
            cdn: Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(60))
                .https_only(true)
                .user_agent("demofetch/0.1")
                .build()?,
            credentials,
            data_base: "https://open.faceit.com/data/v4".into(),
            download_endpoint: "https://open.faceit.com/download/v2/demos/download".into(),
            retries: 3,
            rate: Arc::new(rate_limit::RateGate::new()),
            cdn_rate: Arc::new(rate_limit::RateGate::new()),
            monthly_layout: true,
        })
    }

    async fn request(
        &self,
        method: Method,
        endpoint: &str,
        body: Option<&serde_json::Value>,
        token: &str,
        operation: &str,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<Response> {
        for attempt in 0..=self.retries {
            check_cancel(cancel)?;
            if endpoint == self.download_endpoint && self.rate.download_quota_exhausted().await {
                bail!("FACEIT's advertised download quota is exhausted; signing has stopped. Retry after the quota resets.");
            }
            self.rate.wait(cancel).await?;
            let mut request = self
                .api
                .request(method.clone(), endpoint)
                .bearer_auth(token);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = tokio::select! {
                response = request.send() => response,
                _ = cancel_wait(cancel) => bail!("Canceled"),
            };
            if let Ok(response) = &response {
                self.rate.observe(response.headers()).await;
            }
            let wait_seconds = match response {
                Ok(response) if response.status().is_success() => {
                    self.rate.success().await;
                    return Ok(response);
                }
                Ok(response) => {
                    let status = response.status();
                    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                        return Err(ApiStatus {
                            operation: operation.into(),
                            status,
                        }
                        .into());
                    }
                    if !(status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()) {
                        return Err(ApiStatus {
                            operation: operation.into(),
                            status,
                        }
                        .into());
                    }
                    let delay = rate_limit::retry_after(
                        response
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok()),
                        std::time::SystemTime::now(),
                    )
                    .unwrap_or_else(|| Duration::from_secs(1 << attempt));
                    self.rate
                        .defer(delay, status == StatusCode::TOO_MANY_REQUESTS)
                        .await?;
                    if attempt == self.retries {
                        return Err(ApiStatus {
                            operation: operation.into(),
                            status,
                        }
                        .into());
                    }
                    delay
                        .as_secs()
                        .saturating_add(u64::from(delay.subsec_nanos() != 0))
                }
                Err(error) if attempt == self.retries => return Err(error.without_url().into()),
                Err(_) => {
                    let delay = Duration::from_secs(1 << attempt);
                    self.rate.defer(delay, false).await?;
                    delay.as_secs()
                }
            };
            callback(Event::Retrying {
                operation: operation.into(),
                attempt: attempt + 1,
                wait_seconds,
            });
            // The next attempt goes through the shared gate; every worker sees the cooldown.
        }
        unreachable!()
    }

    async fn cdn_response(
        &self,
        url: &str,
        range: Option<&str>,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<Response> {
        for attempt in 0..=self.retries {
            self.cdn_rate.wait(cancel).await?;
            let mut request = self.cdn.get(url);
            if let Some(range) = range {
                request = request.header("Range", range);
            }
            let response = tokio::select! {
                response = request.send() => response,
                _ = cancel_wait(cancel) => bail!("Canceled"),
            };
            let delay = match response {
                Ok(response) => {
                    self.cdn_rate.observe(response.headers()).await;
                    let status = response.status();
                    if status != StatusCode::TOO_MANY_REQUESTS && !status.is_server_error() {
                        if status.is_success() {
                            self.cdn_rate.success().await;
                        }
                        return Ok(response);
                    }
                    let delay = rate_limit::retry_after(
                        response
                            .headers()
                            .get("retry-after")
                            .and_then(|value| value.to_str().ok()),
                        std::time::SystemTime::now(),
                    )
                    .unwrap_or_else(|| Duration::from_secs(1 << attempt));
                    self.cdn_rate
                        .defer(delay, status == StatusCode::TOO_MANY_REQUESTS)
                        .await?;
                    if attempt == self.retries {
                        return Err(ApiStatus {
                            operation: "Demo CDN request".into(),
                            status,
                        }
                        .into());
                    }
                    delay
                }
                Err(error) if attempt == self.retries => return Err(error.without_url().into()),
                Err(_) => {
                    let delay = Duration::from_secs(1 << attempt);
                    self.cdn_rate.defer(delay, false).await?;
                    delay
                }
            };
            callback(Event::Retrying {
                operation: "Demo CDN request".into(),
                attempt: attempt + 1,
                wait_seconds: delay
                    .as_secs()
                    .saturating_add(u64::from(delay.subsec_nanos() != 0)),
            });
        }
        unreachable!()
    }

    /// Bounded, paginated recent CS2 history; downloading still resolves each match's real demos.
    pub async fn player_matches(
        &self,
        nickname: &str,
        limit: usize,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<Vec<String>> {
        let mut url = Url::parse(&format!("{}/players", self.data_base))?;
        url.query_pairs_mut()
            .append_pair("nickname", nickname)
            .append_pair("game", "cs2");
        let player: serde_json::Value = self
            .request(
                Method::GET,
                url.as_str(),
                None,
                &self.credentials.data_key,
                "Player lookup",
                cancel,
                callback,
            )
            .await?
            .json()
            .await
            .map_err(|e| e.without_url())?;
        let player_id = player["player_id"]
            .as_str()
            .context("FACEIT returned no player_id")?;
        // Do not allow server data to inject URL path or query components.
        let player_id =
            uuid::Uuid::parse_str(player_id).context("Invalid player_id returned by FACEIT")?;
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        let mut offset = 0;
        while offset < limit {
            let page_size = (limit - offset).min(100);
            let endpoint = format!(
                "{}/players/{player_id}/history?game=cs2&offset={offset}&limit={page_size}",
                self.data_base
            );
            let history: serde_json::Value = self
                .request(
                    Method::GET,
                    &endpoint,
                    None,
                    &self.credentials.data_key,
                    "Player history",
                    cancel,
                    callback,
                )
                .await?
                .json()
                .await
                .map_err(|e| e.without_url())?;
            let items = history["items"]
                .as_array()
                .context("FACEIT returned no history items")?;
            for item in items {
                let id = match_id(
                    item["match_id"]
                        .as_str()
                        .context("History item missing match_id")?,
                )?;
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
            if items.len() < page_size {
                break;
            }
            offset += items.len();
        }
        Ok(ids)
    }

    pub async fn download_match(
        &self,
        id: &str,
        output: &Path,
        cancel: &Cancel,
        callback: &Callback,
    ) -> MatchResult {
        let mut result = MatchResult {
            match_id: id.into(),
            files: Vec::new(),
            error: None,
        };
        if let Err(error) = self
            .download_match_inner(id, output, cancel, callback, &mut result)
            .await
        {
            result.error = Some(format!("{error:#}"));
        }
        result
    }

    async fn download_match_inner(
        &self,
        id: &str,
        output: &Path,
        cancel: &Cancel,
        callback: &Callback,
        result: &mut MatchResult,
    ) -> Result<()> {
        let id = match_id(id)?;
        let endpoint = format!("{}/matches/{id}", self.data_base);
        let metadata: serde_json::Value = self
            .request(
                Method::GET,
                &endpoint,
                None,
                &self.credentials.data_key,
                "Match lookup",
                cancel,
                callback,
            )
            .await?
            .json()
            .await
            .map_err(|e| e.without_url())?;
        let urls = metadata["demo_url"]
            .as_array()
            .context("Demo not ready or unavailable: match has no demo_url array")?;
        if urls.is_empty() {
            bail!("Demo not ready or unavailable: match has no demo URLs");
        }
        let mut seen = HashSet::new();
        let mut failures = Vec::new();
        for (index, url) in urls.iter().enumerate() {
            check_cancel(cancel)?;
            let resource = url
                .as_str()
                .context("Invalid demo_url returned by FACEIT")?;
            if !seen.insert(resource) {
                continue;
            }
            let format = match DemoFormat::from_resource(resource) {
                Ok(format) => format,
                Err(error) => {
                    failures.push(format!("Demo {}: {error:#}", index + 1));
                    continue;
                }
            };
            let path = if self.monthly_layout {
                demo_path(output, &id, &metadata, index, format)?
            } else {
                output.join(format!("{id}-demo{}.{}", index + 1, format.extension()))
            };
            tokio::fs::create_dir_all(path.parent().context("Demo has no destination directory")?)
                .await?;
            callback(Event::Downloading {
                match_id: id.clone(),
                demo: index + 1,
                total_demos: urls.len(),
            });
            match self
                .download_one(&id, resource, format, path, cancel, callback)
                .await
            {
                Ok(file) => result.files.push(file),
                Err(error) => failures.push(format!("Demo {}: {error:#}", index + 1)),
            }
        }
        if !failures.is_empty() {
            bail!("{}", failures.join("; "));
        }
        Ok(())
    }

    async fn download_one(
        &self,
        id: &str,
        resource: &str,
        format: DemoFormat,
        path: PathBuf,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<DownloadedDemo> {
        if path.exists() {
            let validation_path = path.clone();
            let (bytes, sha256) =
                tokio::task::spawn_blocking(move || validate_demo(&validation_path, format))
                    .await?
                    .context("Existing demo failed validation; move it aside before retrying")?;
            return Ok(DownloadedDemo {
                path,
                bytes,
                sha256,
                reused: true,
            });
        }
        for attempt in 0..=self.retries {
            check_cancel(cancel)?;
            let response: serde_json::Value = self
                .request(
                    Method::POST,
                    &self.download_endpoint,
                    Some(&serde_json::json!({"resource_url": resource})),
                    &self.credentials.download_token,
                    "Download authorization",
                    cancel,
                    callback,
                )
                .await?
                .json()
                .await
                .map_err(|e| e.without_url())?;
            let signed_url = response["payload"]["download_url"]
                .as_str()
                .context("Downloads API returned no signed download_url")?;
            match self
                .stream_demo(id, signed_url, format, &path, cancel, callback)
                .await
            {
                Ok(file) => return Ok(file),
                Err(error) if attempt == self.retries => return Err(error),
                Err(_) => {
                    check_cancel(cancel)?;
                    let wait_seconds = 1 << attempt;
                    callback(Event::Retrying {
                        operation: "Demo transfer (refreshing signed URL)".into(),
                        attempt: attempt + 1,
                        wait_seconds,
                    });
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(wait_seconds)) => {},
                        _ = cancel_wait(cancel) => bail!("Canceled"),
                    }
                }
            }
        }
        unreachable!()
    }

    async fn stream_demo(
        &self,
        id: &str,
        signed_url: &str,
        format: DemoFormat,
        path: &Path,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<DownloadedDemo> {
        let mut response = self
            .cdn_response(signed_url, None, cancel, callback)
            .await?;
        if response.status() != StatusCode::OK {
            bail!("Demo transfer: HTTP {}", response.status());
        }
        let expected = response.content_length();
        let temporary = tempfile::Builder::new()
            .prefix(".faceit-")
            .suffix(".part")
            .tempfile_in(path.parent().context("Missing output directory")?)?;
        let mut file = tokio::fs::File::from_std(temporary.reopen()?);
        let mut bytes = 0;
        let mut last_progress = std::time::Instant::now();
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk.map_err(|e| e.without_url())?,
                _ = cancel_wait(cancel) => bail!("Canceled"),
            };
            let Some(chunk) = chunk else {
                break;
            };
            file.write_all(&chunk).await?;
            bytes += chunk.len() as u64;
            if last_progress.elapsed() >= Duration::from_millis(250) {
                callback(Event::DownloadProgress {
                    match_id: id.into(),
                    bytes,
                    total_bytes: expected,
                });
                last_progress = std::time::Instant::now();
            }
        }
        if expected.is_some_and(|expected| expected != bytes) {
            bail!("Incomplete demo transfer");
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        // Streaming validation catches HTML/error bodies and truncated gzip/zstd streams without
        // writing or keeping a second, decompressed copy of a multi-GB demo.
        let validation_path = temporary.path().to_owned();
        let (_, sha256) =
            tokio::task::spawn_blocking(move || validate_demo(&validation_path, format)).await??;
        check_cancel(cancel)?;
        temporary
            .persist_noclobber(path)
            .context("Could not publish demo (destination already exists or is unwritable)")?;
        callback(Event::DownloadProgress {
            match_id: id.into(),
            bytes,
            total_bytes: expected,
        });
        Ok(DownloadedDemo {
            path: path.to_owned(),
            bytes,
            sha256,
            reused: false,
        })
    }
}

#[derive(Clone, Copy)]
enum DemoFormat {
    Dem,
    Gzip,
    Zstd,
}

impl DemoFormat {
    fn from_resource(resource: &str) -> Result<Self> {
        let url = Url::parse(resource).context("Invalid demo resource URL")?;
        if url.scheme() != "https" {
            bail!("Demo resource URL must use HTTPS");
        }
        let path = url.path().to_ascii_lowercase();
        if path.ends_with(".dem.gz") {
            Ok(Self::Gzip)
        } else if path.ends_with(".dem.zst") {
            Ok(Self::Zstd)
        } else if path.ends_with(".dem") {
            Ok(Self::Dem)
        } else {
            bail!("Unsupported demo format; expected .dem, .dem.gz or .dem.zst")
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Dem => "dem",
            Self::Gzip => "dem.gz",
            Self::Zstd => "dem.zst",
        }
    }
}

fn validate_demo(path: &Path, format: DemoFormat) -> Result<(u64, String)> {
    let file = std::fs::File::open(path)?;
    let mut reader: Box<dyn Read> = match format {
        DemoFormat::Dem => Box::new(file),
        DemoFormat::Gzip => Box::new(flate2::read::MultiGzDecoder::new(file)),
        DemoFormat::Zstd => Box::new(zstd::stream::read::Decoder::new(file)?),
    };
    let mut magic = [0; 8];
    reader
        .read_exact(&mut magic)
        .context("Empty, truncated, or invalid demo archive")?;
    if &magic != b"PBDEMS2\0" {
        bail!("Downloaded content is not a CS2 demo");
    }
    let decoded_bytes = std::io::copy(&mut reader, &mut std::io::sink())
        .context("Demo archive is truncated or corrupt")?;
    if decoded_bytes == 0 {
        bail!("Demo contains only a signature and no payload");
    }
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hasher.update(&buffer[..size]);
        bytes += size as u64;
    }
    Ok((bytes, format!("{:x}", hasher.finalize())))
}

/// Concurrent match downloads with bounded buffering. The receiver can start parsing as soon as
/// the first match finishes; it must serialize parser calls to avoid concurrent catalog writers.
pub async fn download_batch(
    client: FaceitClient,
    ids: Vec<String>,
    output: PathBuf,
    concurrency: usize,
    cancel: Cancel,
    callback: Callback,
    ready: tokio::sync::mpsc::Sender<MatchResult>,
) -> Result<()> {
    if !(1..=16).contains(&concurrency) {
        bail!("Download concurrency must be between 1 and 16");
    }
    callback(Event::Started { matches: ids.len() });
    let mut jobs = tokio::task::JoinSet::new();
    let mut ids = ids.into_iter();
    loop {
        while jobs.len() < concurrency && !cancel.load(Ordering::Relaxed) {
            let Some(id) = ids.next() else {
                break;
            };
            let (client, output, cancel, callback) = (
                client.clone(),
                output.clone(),
                cancel.clone(),
                callback.clone(),
            );
            jobs.spawn(async move {
                client
                    .download_match(&id, &output, &cancel, &callback)
                    .await
            });
        }
        let Some(result) = jobs.join_next().await else {
            break;
        };
        let result = result.context("Download worker failed")?;
        callback(Event::MatchCompleted {
            result: result.clone(),
        });
        // A bounded channel prevents downloading an unbounded backlog ahead of a slow parser.
        tokio::select! {
            sent = ready.send(result) => sent.context("Download result receiver closed")?,
            _ = cancel_wait(&cancel) => break,
        }
    }
    Ok(())
}

/// Write a durable per-run report with no tokens, signed URLs, or source URLs.
pub fn save_report(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("Report needs a parent directory")?)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .context("Could not save download report")?;
    Ok(())
}

#[cfg(test)]
mod tests;
