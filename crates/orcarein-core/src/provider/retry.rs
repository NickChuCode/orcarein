//! Retry transient provider failures with exponential backoff + jitter.
//!
//! Wraps only the *pre-stream* phase of an HTTP call — connect, response
//! headers, status — so a retried attempt never double-emits streamed tokens
//! (OpenAI-compatible SSE has no resumption). Once bytes flow, errors are the
//! caller's to surface, not ours to retry.
//!
//! Silent by design (no `tracing`), mirroring `memory.rs` / `skill.rs`.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;
use reqwest::StatusCode;

/// How many times to retry, how long to back off, and the per-attempt ceiling
/// on the pre-stream phase. Only `max_retries` is user-configurable; the rest
/// are constants (see the P0.1 spec).
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub request_timeout: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// Builds a policy from the optional configured `max_retries`, clamped to
    /// `[0, 10]` (a pathological large value blows up worst-case latency and
    /// the shift regime). `None` → the default (3). Other fields stay constant.
    pub fn from_config(max_retries: Option<u32>) -> Self {
        let mut p = RetryPolicy::default();
        if let Some(n) = max_retries {
            p.max_retries = n.min(10);
        }
        p
    }
}

/// Whether a failed attempt should be retried, and any server-suggested delay.
pub(super) enum Decision {
    Retryable(Option<Duration>),
    Fatal,
}

/// A classified failure from one attempt of the wrapped operation.
pub(super) enum RetryError {
    Retryable {
        source: anyhow::Error,
        retry_after: Option<Duration>,
    },
    Fatal(anyhow::Error),
}

/// Exponential backoff for `attempt` (0-based): `base · 2^attempt`, capped at
/// `max_delay`. Shifts the multiplier `1u64` (not the base) and saturates, so
/// large `attempt` values cap out instead of wrapping.
pub(super) fn backoff(policy: &RetryPolicy, attempt: u32) -> Duration {
    let base_ms = policy.base_delay.as_millis() as u64;
    let max_ms = policy.max_delay.as_millis() as u64;
    let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    let ms = base_ms.saturating_mul(factor).min(max_ms);
    Duration::from_millis(ms)
}

/// Full jitter: a point in `[0, ceiling_ms]` selected by `entropy`. Pure and
/// deterministic in its inputs, so it is unit-testable.
pub(super) fn jitter(ceiling_ms: u64, entropy: u64) -> Duration {
    if ceiling_ms == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(entropy % (ceiling_ms + 1))
}

/// `jitter` seeded from the wall clock (immune to tokio's paused time, so it
/// never perturbs `start_paused` tests). Hand-rolled, zero-dep (no `rand`).
fn jittered_delay(policy: &RetryPolicy, attempt: u32) -> Duration {
    let ceiling = backoff(policy, attempt).as_millis() as u64;
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    jitter(ceiling, entropy)
}

/// Parses a `Retry-After` header as integer delta-seconds. Non-numeric forms
/// (e.g. an HTTP-date) are ignored — the caller falls back to computed backoff.
fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    raw.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Status → decision. 429 / 408 / 5xx are retryable (with `Retry-After` when
/// present); every other status (400/401/403/404/…) is fatal.
pub(super) fn classify_status(status: StatusCode, headers: &HeaderMap) -> Decision {
    let retryable = matches!(status.as_u16(), 408 | 429) || status.is_server_error();
    if retryable {
        Decision::Retryable(parse_retry_after(headers))
    } else {
        Decision::Fatal
    }
}

/// Transport-error retryability. Everything except a malformed request or a
/// redirect loop is a transient transport failure worth retrying (pre-response
/// connection resets don't always set `is_connect()`).
pub(super) fn retryability(is_request: bool, is_redirect: bool) -> Decision {
    if is_request || is_redirect {
        Decision::Fatal
    } else {
        Decision::Retryable(None)
    }
}

/// One-line adapter over a real `reqwest::Error` (untestable directly — no
/// public constructor — so the decision lives in the pure `retryability`).
pub(super) fn classify_reqwest(e: &reqwest::Error) -> Decision {
    retryability(e.is_request(), e.is_redirect())
}

/// Runs `op`, retrying transient failures with backoff (or the server's
/// `Retry-After`, capped at `max_delay`) up to `max_retries` times. `Fatal`
/// returns immediately without sleeping.
pub(super) async fn with_retry<T, F, Fut>(policy: &RetryPolicy, mut op: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, RetryError>>,
{
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(RetryError::Fatal(e)) => return Err(e),
            Err(RetryError::Retryable {
                source,
                retry_after,
            }) => {
                if attempt >= policy.max_retries {
                    return Err(
                        source.context(format!("gave up after {} retries", policy.max_retries))
                    );
                }
                let delay = retry_after
                    .map(|d| d.min(policy.max_delay))
                    .unwrap_or_else(|| jittered_delay(policy, attempt));
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy {
            max_retries,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
            request_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let p = policy(3);
        assert_eq!(backoff(&p, 0), Duration::from_millis(500));
        assert_eq!(backoff(&p, 1), Duration::from_millis(1000));
        assert_eq!(backoff(&p, 2), Duration::from_millis(2000));
        assert_eq!(backoff(&p, 3), Duration::from_millis(4000));
        assert_eq!(backoff(&p, 4), Duration::from_secs(8)); // capped
                                                            // Overflow regime must saturate to the cap, never wrap.
        assert_eq!(backoff(&p, 64), Duration::from_secs(8));
        assert_eq!(backoff(&p, 100), Duration::from_secs(8));
    }

    #[test]
    fn jitter_stays_within_ceiling() {
        assert_eq!(jitter(8000, 0), Duration::ZERO);
        assert_eq!(jitter(8000, 8000), Duration::from_millis(8000));
        assert_eq!(jitter(8000, 8001), Duration::ZERO);
        assert_eq!(jitter(0, 12345), Duration::ZERO);
        for e in [1u64, 42, 7999, u64::MAX] {
            assert!(jitter(8000, e) <= Duration::from_millis(8000));
        }
    }

    #[test]
    fn classify_status_retryable_and_fatal() {
        let h = HeaderMap::new();
        for code in [429u16, 408, 500, 503] {
            assert!(matches!(
                classify_status(StatusCode::from_u16(code).unwrap(), &h),
                Decision::Retryable(_)
            ));
        }
        for code in [400u16, 401, 403, 404] {
            assert!(matches!(
                classify_status(StatusCode::from_u16(code).unwrap(), &h),
                Decision::Fatal
            ));
        }
    }

    #[test]
    fn classify_status_parses_retry_after() {
        let mut h = HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        match classify_status(StatusCode::TOO_MANY_REQUESTS, &h) {
            Decision::Retryable(Some(d)) => assert_eq!(d, Duration::from_secs(3)),
            _ => panic!("expected Retryable(Some(3s))"),
        }
        // HTTP-date form → ignored → fall back to None.
        let mut h2 = HeaderMap::new();
        h2.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert!(matches!(
            classify_status(StatusCode::TOO_MANY_REQUESTS, &h2),
            Decision::Retryable(None)
        ));
    }

    #[test]
    fn retryability_only_request_and_redirect_are_fatal() {
        assert!(matches!(
            retryability(false, false),
            Decision::Retryable(None)
        ));
        assert!(matches!(retryability(true, false), Decision::Fatal));
        assert!(matches!(retryability(false, true), Decision::Fatal));
        assert!(matches!(retryability(true, true), Decision::Fatal));
    }

    #[test]
    fn from_config_clamps_and_defaults() {
        assert_eq!(RetryPolicy::from_config(None).max_retries, 3);
        assert_eq!(RetryPolicy::from_config(Some(5)).max_retries, 5);
        assert_eq!(RetryPolicy::from_config(Some(999)).max_retries, 10);
        assert_eq!(RetryPolicy::from_config(Some(0)).max_retries, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_then_succeeds() {
        let calls = Cell::new(0u32);
        let p = policy(3);
        let r: anyhow::Result<u32> = with_retry(&p, || {
            let n = calls.get();
            calls.set(n + 1);
            async move {
                if n < 2 {
                    Err(RetryError::Retryable {
                        source: anyhow::anyhow!("429"),
                        retry_after: None,
                    })
                } else {
                    Ok(42)
                }
            }
        })
        .await;
        assert_eq!(r.unwrap(), 42);
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_max_retries() {
        let calls = Cell::new(0u32);
        let p = policy(3);
        let r: anyhow::Result<u32> = with_retry(&p, || {
            calls.set(calls.get() + 1);
            async {
                Err(RetryError::Retryable {
                    source: anyhow::anyhow!("503"),
                    retry_after: None,
                })
            }
        })
        .await;
        assert!(r.is_err());
        assert_eq!(calls.get(), 4); // 1 initial + 3 retries
    }

    #[tokio::test(start_paused = true)]
    async fn fatal_does_not_retry() {
        let calls = Cell::new(0u32);
        let p = policy(3);
        let r: anyhow::Result<u32> = with_retry(&p, || {
            calls.set(calls.get() + 1);
            async { Err(RetryError::Fatal(anyhow::anyhow!("400"))) }
        })
        .await;
        assert!(r.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn honors_retry_after() {
        let calls = Cell::new(0u32);
        let p = policy(3);
        let r: anyhow::Result<u32> = with_retry(&p, || {
            let n = calls.get();
            calls.set(n + 1);
            async move {
                if n == 0 {
                    Err(RetryError::Retryable {
                        source: anyhow::anyhow!("429"),
                        retry_after: Some(Duration::from_secs(3)),
                    })
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(r.unwrap(), 7);
        assert_eq!(calls.get(), 2);
    }
}
