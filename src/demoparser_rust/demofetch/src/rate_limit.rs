//! One pacing/cooldown gate shared by every API request and cloned batch worker.
use crate::{cancel_wait, check_cancel, Cancel};
use anyhow::{bail, Context, Result};
use std::time::{Duration, SystemTime};
use tokio::{sync::Mutex, time::Instant};

pub fn retry_after(value: Option<&str>, now: SystemTime) -> Option<Duration> {
    let value = value?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|date| date.duration_since(now).unwrap_or_default())
}

struct State {
    next: Instant,
    spacing: Duration,
    baseline: Duration,
    successes: usize,
    quota_blocked: bool,
}
pub struct RateGate {
    state: Mutex<State>,
}
impl RateGate {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                next: Instant::now(),
                spacing: Duration::from_millis(100),
                baseline: Duration::from_millis(100),
                successes: 0,
                quota_blocked: false,
            }),
        }
    }
    pub async fn download_quota_exhausted(&self) -> bool {
        self.state.lock().await.quota_blocked
    }

    pub async fn observe(&self, headers: &reqwest::header::HeaderMap) {
        let number = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
        };
        let mut state = self.state.lock().await;
        if let Some(baseline) = headers
            .get("ratelimit-limit")
            .and_then(|value| value.to_str().ok())
            .and_then(advertised_spacing)
        {
            if state.spacing == state.baseline {
                state.spacing = baseline;
            } else {
                state.spacing = state.spacing.max(baseline);
            }
            state.baseline = baseline;
        }
        if number("ratelimit-remaining").is_some_and(|remaining| remaining <= 1) {
            let delay = Duration::from_secs(number("ratelimit-reset").unwrap_or(1).max(1))
                + Duration::from_millis(100);
            if let Some(deadline) = Instant::now().checked_add(delay) {
                state.next = state.next.max(deadline);
            }
        }
        for (total, used) in [
            (
                "x-faceit-downloadquota-total",
                "x-faceit-downloadquota-used",
            ),
            (
                "x-faceit-downloadquota-bytes-total",
                "x-faceit-downloadquota-bytes-used",
            ),
        ] {
            // Negative sentinel values are not treated as a finite quota.
            if let (Some(total), Some(used)) = (number(total), number(used)) {
                if used >= total {
                    state.quota_blocked = true;
                }
            }
        }
    }
    pub async fn wait(&self, cancel: &Cancel) -> Result<()> {
        loop {
            check_cancel(cancel)?;
            let deadline = {
                let mut state = self.state.lock().await;
                if Instant::now() >= state.next {
                    state.next = Instant::now() + state.spacing;
                    return Ok(());
                }
                state.next
            };
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {},
                _ = cancel_wait(cancel) => bail!("Canceled"),
            }
            // Recheck: a different worker may have extended the shared cooldown while we slept.
        }
    }
    pub async fn defer(&self, delay: Duration, limited: bool) -> Result<()> {
        let jitter = Duration::from_millis(50 + (uuid::Uuid::new_v4().as_u128() % 201) as u64);
        let until = Instant::now()
            .checked_add(delay)
            .and_then(|deadline| deadline.checked_add(jitter))
            .context("Retry-After exceeds supported clock range")?;
        let mut state = self.state.lock().await;
        state.next = state.next.max(until);
        state.successes = 0;
        if limited {
            state.spacing = (state.spacing * 2)
                .min(Duration::from_secs(2))
                .max(state.baseline);
        }
        Ok(())
    }
    pub async fn success(&self) {
        let mut state = self.state.lock().await;
        state.successes += 1;
        if state.successes >= 20 {
            state.spacing = state.spacing.mul_f64(0.9).max(state.baseline);
            state.successes = 0;
        }
    }
}

/// Use the tightest advertised window with 20% headroom. Do not guess a window for bare limits.
fn advertised_spacing(value: &str) -> Option<Duration> {
    value
        .split(',')
        .filter_map(|policy| {
            let mut fields = policy.trim().split(';');
            let limit = fields.next()?.trim().parse::<u64>().ok()?;
            let window = fields.find_map(|field| {
                field
                    .trim()
                    .strip_prefix("w=")
                    .and_then(|window| window.trim_matches('"').parse::<u64>().ok())
            })?;
            if limit == 0 || window == 0 {
                return None;
            }
            let seconds = window as f64 / (limit as f64 * 0.8);
            Duration::try_from_secs_f64(seconds).ok()
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    #[tokio::test(start_paused = true)]
    async fn advertised_windows_and_remaining_budget_prevent_early_requests() {
        let gate = RateGate::new();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("ratelimit-limit", "20, 20;w=1, 100;w=60".parse().unwrap());
        headers.insert("ratelimit-remaining", "0".parse().unwrap());
        headers.insert("ratelimit-reset", "2".parse().unwrap());
        gate.observe(&headers).await;
        assert_eq!(gate.state.lock().await.baseline, Duration::from_millis(750));
        let start = Instant::now();
        gate.wait(&Arc::new(AtomicBool::new(false))).await.unwrap();
        assert!(start.elapsed() >= Duration::from_millis(2100));
    }
    #[tokio::test]
    async fn finite_download_quotas_stop_signing_negative_sentinels_do_not() {
        let gate = RateGate::new();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-faceit-downloadquota-total", "-1".parse().unwrap());
        headers.insert("x-faceit-downloadquota-used", "100".parse().unwrap());
        gate.observe(&headers).await;
        assert!(!gate.download_quota_exhausted().await);
        headers.insert("x-faceit-downloadquota-total", "100".parse().unwrap());
        gate.observe(&headers).await;
        assert!(gate.download_quota_exhausted().await);
    }
}
