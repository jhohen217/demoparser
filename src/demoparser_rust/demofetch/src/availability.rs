//! Availability is measured, never inferred from a fixed retention period. The date boundary is
//! a search heuristic: isolated missing uploads and surviving older archives are possible.
use crate::*;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Candidate {
    pub match_id: String,
    pub finished_at: u64,
    /// True for legacy queues containing only a calendar date, rather than an exact finish time.
    #[serde(default)]
    pub date_is_hint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum Availability {
    Available,
    Saved,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub candidate: Candidate,
    pub outcome: Availability,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchReport {
    pub checked_at: u64,
    pub candidate_count: usize,
    pub probes: Vec<Observation>,
    pub oldest_confirmed_available: Option<Candidate>,
    pub older_unchecked: usize,
    pub queue: Vec<String>,
    pub unavailable: Vec<String>,
    pub probe_budget: usize,
    pub budget_exhausted: bool,
    pub scope: String,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn timestamp(item: &serde_json::Value) -> Result<u64> {
    item["finished_at"]
        .as_u64()
        .filter(|value| *value > 0)
        .context("Match has no finished_at timestamp; cannot order it by expiry risk")
}

impl FaceitClient {
    /// Retrieve the complete date range. Split dense intervals instead of exceeding FACEIT's
    /// documented history offset limit (1000), and deduplicate overlapping interval edges.
    pub async fn availability_history(
        &self,
        nickname: &str,
        days: u32,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<Vec<Candidate>> {
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
        let player_id =
            uuid::Uuid::parse_str(player["player_id"].as_str().context("Missing player_id")?)?;
        let to = now();
        let from = to.saturating_sub(days as u64 * 86400);
        let mut intervals = vec![(from, to)];
        let mut all = BTreeMap::new();
        while let Some((from, to)) = intervals.pop() {
            let mut page_items = Vec::new();
            let mut dense = false;
            for offset in (0..=1000).step_by(100) {
                let endpoint = format!("{}/players/{player_id}/history?game=cs2&from={from}&to={to}&offset={offset}&limit=100", self.data_base);
                let history: serde_json::Value = self
                    .request(
                        Method::GET,
                        &endpoint,
                        None,
                        &self.credentials.data_key,
                        "Availability history",
                        cancel,
                        callback,
                    )
                    .await?
                    .json()
                    .await
                    .map_err(|e| e.without_url())?;
                let items = history["items"]
                    .as_array()
                    .context("Missing history items")?;
                page_items.extend(items.iter().cloned());
                if items.len() < 100 {
                    break;
                }
                dense = offset == 1000;
            }
            if dense {
                if to.saturating_sub(from) <= 1 {
                    bail!("History contains too many matches at one timestamp; refusing to silently omit candidates");
                }
                let middle = from + (to - from) / 2;
                intervals.push((from, middle));
                intervals.push((middle, to));
                continue;
            }
            for item in page_items {
                // Pending matches have no completed demo yet. They are outside the expiry search.
                if item["finished_at"].as_u64().unwrap_or(0) == 0 {
                    continue;
                }
                let id = match_id(
                    item["match_id"]
                        .as_str()
                        .context("History item missing match ID")?,
                )?;
                all.insert(
                    id.clone(),
                    Candidate {
                        match_id: id,
                        finished_at: timestamp(&item)?,
                        date_is_hint: false,
                    },
                );
            }
            if all.len() > 50_000 {
                bail!("More than 50000 history candidates; narrow --search-days to keep the search complete and bounded");
            }
        }
        let mut candidates: Vec<_> = all.into_values().collect();
        candidates.sort_by(|a, b| (a.finished_at, &a.match_id).cmp(&(b.finished_at, &b.match_id)));
        Ok(candidates)
    }

    pub async fn dated_matches(
        &self,
        ids: &[String],
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<Vec<Candidate>> {
        let mut candidates = Vec::new();
        for id in ids {
            let metadata = self.availability_metadata(id, cancel, callback).await?;
            candidates.push(Candidate {
                match_id: id.clone(),
                finished_at: timestamp(&metadata)?,
                date_is_hint: false,
            });
        }
        Ok(candidates)
    }

    async fn availability_metadata(
        &self,
        id: &str,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<serde_json::Value> {
        Ok(self
            .request(
                Method::GET,
                &format!("{}/matches/{id}", self.data_base),
                None,
                &self.credentials.data_key,
                "Availability match lookup",
                cancel,
                callback,
            )
            .await?
            .json()
            .await
            .map_err(|e| e.without_url())?)
    }

    /// A signed URL alone does not prove the stored object still exists. Probe a small range
    /// directly from the CDN and drop the response after reading its format prefix.
    pub async fn probe_available(
        &self,
        candidate: &Candidate,
        output: &Path,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<Availability> {
        let metadata = match self
            .availability_metadata(&candidate.match_id, cancel, callback)
            .await
        {
            Ok(value) => value,
            Err(error) if missing(&error) => {
                return Ok(Availability::Unknown("Match metadata unavailable".into()))
            }
            Err(error) => return Err(error),
        };
        let Some(urls) = metadata["demo_url"]
            .as_array()
            .filter(|urls| !urls.is_empty())
        else {
            return Ok(Availability::Unknown(
                "Demo URLs absent; not proof of expiry".into(),
            ));
        };
        let mut available = false;
        let mut saved = 0;
        let mut unknown = false;
        for (index, resource) in urls.iter().enumerate() {
            check_cancel(cancel)?;
            let Some(resource) = resource.as_str() else {
                unknown = true;
                continue;
            };
            let format = match DemoFormat::from_resource(resource) {
                Ok(format) => format,
                Err(_) => {
                    unknown = true;
                    continue;
                }
            };
            let path = if self.monthly_layout {
                demo_path(output, &candidate.match_id, &metadata, index, format)?
            } else {
                output.join(format!(
                    "{}-demo{}.{}",
                    candidate.match_id,
                    index + 1,
                    format.extension()
                ))
            };
            if path.is_file() {
                let is_valid =
                    tokio::task::spawn_blocking(move || validate_demo(&path, format).is_ok())
                        .await?;
                if is_valid {
                    saved += 1;
                    continue;
                }
            }
            let signed: serde_json::Value = match self
                .request(
                    Method::POST,
                    &self.download_endpoint,
                    Some(&serde_json::json!({"resource_url":resource})),
                    &self.credentials.download_token,
                    "Availability authorization",
                    cancel,
                    callback,
                )
                .await
            {
                Ok(response) => response.json().await.map_err(|e| e.without_url())?,
                Err(error) if missing(&error) => continue,
                Err(error) => return Err(error),
            };
            let url = signed["payload"]["download_url"]
                .as_str()
                .context("Availability signing returned no download_url")?;
            let mut response = self
                .cdn_response(url, Some("bytes=0-7"), cancel, callback)
                .await?;
            let status = response.status();
            if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                continue;
            }
            // Access failures and exhausted rate limits must never move the expiry boundary.
            if status != StatusCode::OK && status != StatusCode::PARTIAL_CONTENT {
                return Err(ApiStatus {
                    operation: "Availability CDN probe".into(),
                    status,
                }
                .into());
            }
            let mut prefix = Vec::new();
            while prefix.len() < 8 {
                let chunk = tokio::select! {
                    chunk = response.chunk() => chunk.map_err(|e| e.without_url())?,
                    _ = cancel_wait(cancel) => bail!("Canceled"),
                };
                let Some(chunk) = chunk else {
                    break;
                };
                prefix.extend_from_slice(&chunk[..chunk.len().min(8 - prefix.len())]);
            }
            let valid = match format {
                DemoFormat::Dem => prefix == b"PBDEMS2\0",
                DemoFormat::Gzip => prefix.starts_with(&[0x1f, 0x8b]),
                DemoFormat::Zstd => prefix.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]),
            };
            if valid {
                available = true;
            } else {
                unknown = true;
            }
        }
        Ok(if available {
            Availability::Available
        } else if saved == urls.len() {
            Availability::Saved
        } else if unknown || saved > 0 {
            Availability::Unknown("Some maps are saved or availability is inconclusive".into())
        } else {
            Availability::Unavailable
        })
    }

    pub async fn find_oldest_available(
        &self,
        candidates: Vec<Candidate>,
        output: &Path,
        limit: usize,
        budget: usize,
        scope: String,
        cancel: &Cancel,
        callback: &Callback,
    ) -> Result<SearchReport> {
        search(
            candidates,
            limit,
            budget,
            scope,
            |candidate| {
                let candidate = candidate.clone();
                let client = self.clone();
                let output = output.to_owned();
                let cancel = cancel.clone();
                let callback = callback.clone();
                async move {
                    client
                        .probe_available(&candidate, &output, &cancel, &callback)
                        .await
                }
            },
            callback,
        )
        .await
    }
}

fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ApiStatus>()
        .is_some_and(|error| matches!(error.status, StatusCode::NOT_FOUND | StatusCode::GONE))
}

/// Exponential jumps from newest find a cold/warm bracket, binary refinement narrows it. Older scouts and a
/// sequential fringe protect against holes. Unchecked older rows remain explicit in the report.
pub async fn search<F, Fut>(
    mut candidates: Vec<Candidate>,
    limit: usize,
    budget: usize,
    scope: String,
    mut probe: F,
    callback: &Callback,
) -> Result<SearchReport>
where
    F: FnMut(&Candidate) -> Fut,
    Fut: std::future::Future<Output = Result<Availability>>,
{
    if budget < 2 || limit == 0 {
        bail!("Availability search needs at least 2 probes and a positive queue limit");
    }
    candidates.sort_by(|a, b| (a.finished_at, &a.match_id).cmp(&(b.finished_at, &b.match_id)));
    let mut seen = HashSet::new();
    candidates.retain(|c| seen.insert(c.match_id.clone()));
    let n = candidates.len();
    let mut observed: BTreeMap<usize, Availability> = BTreeMap::new();
    // The algorithm is adaptive but all updates happen only after a real classified probe.
    let mut pending = if n > 0 { Some(n - 1) } else { None };
    let mut step = 1usize;
    let mut stage = 0;
    let mut scouts = 0;
    let mut fringe = 0;
    while observed.len() < budget {
        let Some(index) = pending else {
            break;
        };
        if !observed.contains_key(&index) {
            let outcome = probe(&candidates[index]).await?;
            callback(Event::AvailabilityProbe {
                match_id: candidates[index].match_id.clone(),
                finished_at: candidates[index].finished_at,
                outcome: outcome.clone(),
                probes: observed.len() + 1,
                budget,
            });
            observed.insert(index, outcome);
        }
        let warm = observed
            .iter()
            .find_map(|(i, state)| (*state == Availability::Available).then_some(*i));
        let cold = warm.and_then(|warm| {
            observed
                .range(..warm)
                .rev()
                .find_map(|(i, state)| (*state == Availability::Unavailable).then_some(*i))
        });
        if stage == 0 {
            if cold.is_some() && warm.is_some() {
                stage = 1;
            } else if index > 0 {
                let next = (n - 1).saturating_sub(step);
                step = step.saturating_mul(2);
                pending = Some(next);
                continue;
            } else {
                stage = 1;
            }
        }
        if stage == 1 {
            if let Some(warm) = warm {
                let low = cold.map(|cold| cold + 1).unwrap_or(0);
                if low < warm {
                    let middle = low + (warm - low) / 2;
                    let next = (low..warm)
                        .filter(|i| !observed.contains_key(i))
                        .min_by_key(|i| i.abs_diff(middle));
                    if next.is_some() {
                        pending = next;
                        continue;
                    }
                }
            }
            stage = 2;
        }
        if stage == 2 {
            if let Some(warm) = warm {
                if warm > 0 && scouts < 8 {
                    let target = warm.saturating_mul(scouts) / 8;
                    scouts += 1;
                    let next = (0..warm)
                        .filter(|i| !observed.contains_key(i))
                        .min_by_key(|i| i.abs_diff(target));
                    if next.is_some() {
                        pending = next;
                        stage = 1;
                        continue;
                    }
                }
                fringe = warm.saturating_sub(4);
            }
            stage = 3;
        }
        let queued = observed
            .values()
            .filter(|state| **state == Availability::Available)
            .count();
        // Search the oldest confirmed area first. Even when sample probes already found enough
        // candidates, fill/verify earlier rows before accepting a newer sampled candidate.
        let last_queued = observed
            .iter()
            .filter(|(_, state)| **state == Availability::Available)
            .nth(limit.saturating_sub(1))
            .map(|(i, _)| *i);
        let end = if queued >= limit {
            last_queued.map(|i| i + 1).unwrap_or(n)
        } else {
            n
        };
        pending = (fringe..end)
            .find(|i| !observed.contains_key(i))
            .or_else(|| {
                // A full queue does not prove older unprobed survivors are gone. Spend remaining
                // budget on older exceptions rather than leaving useful probe capacity idle.
                warm.and_then(|warm| (0..warm).find(|i| !observed.contains_key(i)))
            });
    }
    let oldest = observed
        .iter()
        .find_map(|(i, state)| (*state == Availability::Available).then_some(*i));
    let queue = observed
        .iter()
        .filter(|(_, state)| **state == Availability::Available)
        .take(limit)
        .map(|(i, _)| candidates[*i].match_id.clone())
        .collect();
    let unavailable = observed
        .iter()
        .filter(|(_, state)| **state == Availability::Unavailable)
        .map(|(i, _)| candidates[*i].match_id.clone())
        .collect();
    let report = SearchReport {
        checked_at: now(),
        candidate_count: n,
        older_unchecked: oldest
            .map(|oldest| (0..oldest).filter(|i| !observed.contains_key(i)).count())
            .unwrap_or(n.saturating_sub(observed.len())),
        oldest_confirmed_available: oldest.map(|i| candidates[i].clone()),
        queue,
        unavailable,
        budget_exhausted: observed.len() == budget && observed.len() < n,
        probe_budget: budget,
        probes: observed
            .into_iter()
            .map(|(i, outcome)| Observation {
                candidate: candidates[i].clone(),
                outcome,
            })
            .collect(),
        scope,
    };
    callback(Event::AvailabilitySearchFinished {
        report: report.clone(),
    });
    Ok(report)
}

/// Remove only positively classified missing resources. Successful local demos are never deleted.
pub fn purge_queue(path: &Path, unavailable: &[String]) -> Result<()> {
    let remove: HashSet<_> = unavailable.iter().collect();
    let original = std::fs::read_to_string(path)?;
    let mut cleaned = String::new();
    let mut changed = false;
    for raw_line in original.split_inclusive('\n') {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        let (entries, comment) = line
            .split_once('#')
            .map(|(entries, comment)| (entries, Some(comment)))
            .unwrap_or((line, None));
        let parsed = input_matches(entries)?;
        if !parsed.iter().any(|entry| remove.contains(&entry.match_id)) {
            cleaned.push_str(raw_line);
            continue;
        }
        changed = true;
        let mut retained = Vec::new();
        let mut tokens = entries
            .trim_start_matches('\u{feff}')
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|v| !v.is_empty())
            .peekable();
        while let Some(token) = tokens.next() {
            let id = match_id(token)?;
            let value = if tokens
                .peek()
                .is_some_and(|value| queue_timestamp(value).is_some())
            {
                format!("{token},{}", tokens.next().unwrap())
            } else {
                token.to_owned()
            };
            if !remove.contains(&id) {
                retained.push(value);
            }
        }
        cleaned.push_str(&retained.join(" "));
        if let Some(comment) = comment {
            cleaned.push('#');
            cleaned.push_str(comment);
        }
        if raw_line.ends_with("\r\n") {
            cleaned.push_str("\r\n");
        } else if raw_line.ends_with('\n') {
            cleaned.push('\n');
        }
    }
    if !changed {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(cleaned.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .context("Could not update expired-link queue")?;
    Ok(())
}
