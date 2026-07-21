//! Token bucket rate limiter
//! Provides proactive throttling to prevent exceeding API limits.

use crate::config::RateLimitConfig;
use crate::utils::cancel::CancelGuard;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::debug;

/// Smallest wait we will ever sleep before retrying, to avoid tight loops when
/// two tasks wake near-simultaneously and race for the next token.
const MIN_WAIT: Duration = Duration::from_micros(500);

/// Largest single sleep slice when waiting for a token under cancellation.
/// Bounds the worst-case latency between a `cancel_all` call and the cancelled
/// waiter actually returning. Picked at 50 ms because that is well under any
/// human-perceptible delay and the rate limiter never holds tokens for longer
/// than a second under the default config.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct RateLimiter {
    tokens: Arc<Mutex<TokenBucket>>,
}

/// Multiplicative-decrease factor applied to the refill rate on every
/// observed 429, and the additive per-success recovery step back toward
/// the configured ceiling. Classic AIMD: back off hard when the upstream
/// says stop, creep back while it stays quiet. The floor keeps the
/// limiter serviceable even under a hostile upstream.
const AIMD_DECREASE_FACTOR: f64 = 0.5;
const AIMD_RECOVERY_STEP: f64 = 0.1;
const AIMD_MIN_RATE: f64 = 1.0;

struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_rate: f64,
    /// The configured ceiling `refill_rate` recovers toward. Never
    /// exceeded — AIMD adapts downward from here, not upward past it.
    configured_rate: f64,
    last_refill: Instant,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        let capacity = config.burst_size as f64;
        let rate = config.requests_per_second as f64;

        let bucket = TokenBucket {
            tokens: capacity,
            capacity,
            refill_rate: rate,
            configured_rate: rate,
            last_refill: Instant::now(),
        };

        Self {
            tokens: Arc::new(Mutex::new(bucket)),
        }
    }

    /// Acquire a token, waiting if necessary.
    pub async fn acquire(&self) {
        loop {
            match self.try_consume_or_wait() {
                Ok(()) => return,
                Err(wait) => tokio::time::sleep(wait.max(MIN_WAIT)).await,
            }
        }
    }

    /// Acquire a token, waiting if necessary, but bail out if `guard` flips to
    /// cancelled while we're waiting. Returns `true` on a successful acquire,
    /// `false` if cancellation observed before a token was available.
    ///
    /// Long sleeps are sliced into [`CANCEL_POLL_INTERVAL`]-sized pieces so a
    /// `cancel_all` call propagates within ~50 ms regardless of how full the
    /// bucket is. Without slicing, a queued waiter would hold the cancellation
    /// off until its full computed wait elapsed.
    pub async fn acquire_cancellable(&self, guard: &CancelGuard) -> bool {
        loop {
            if guard.is_cancelled() {
                return false;
            }
            match self.try_consume_or_wait() {
                Ok(()) => return true,
                Err(wait) => {
                    let slice = wait.max(MIN_WAIT).min(CANCEL_POLL_INTERVAL);
                    tokio::time::sleep(slice).await;
                }
            }
        }
    }

    /// Try to acquire a token without blocking; returns true on success.
    pub fn try_acquire(&self) -> bool {
        self.try_consume_or_wait().is_ok()
    }

    /// Empty the bucket AND multiplicatively cut the refill rate. Called
    /// when the upstream returns a 429: the accumulated burst headroom is
    /// exactly what re-trips the upstream the moment its retry window
    /// clears — up to `burst_size` queued callers fire back-to-back into a
    /// freshly-reset quota. Draining makes the post-window resume start
    /// from zero tokens; the rate cut (AIMD decrease, floored at
    /// [`AIMD_MIN_RATE`]) makes it resume *slower* than the pace that
    /// tripped the limit, recovering via [`Self::reward`] as successes
    /// accumulate.
    pub fn drain(&self) {
        let mut bucket = self.tokens.lock();
        bucket.tokens = 0.0;
        bucket.last_refill = Instant::now();
        bucket.refill_rate = (bucket.refill_rate * AIMD_DECREASE_FACTOR).max(AIMD_MIN_RATE);
        debug!(rate = bucket.refill_rate, "Token bucket drained + rate cut after upstream rate-limit signal");
    }

    /// Additive recovery toward the configured ceiling, called on every
    /// successful upstream response. A sustained quiet run walks the rate
    /// back up (0.1 req/s per success); a fresh 429 halves it again.
    pub fn reward(&self) {
        let mut bucket = self.tokens.lock();
        if bucket.refill_rate < bucket.configured_rate {
            bucket.refill_rate =
                (bucket.refill_rate + AIMD_RECOVERY_STEP).min(bucket.configured_rate);
        }
    }

    /// Current adaptive refill rate (req/s). Diagnostic accessor.
    pub fn current_rate(&self) -> f64 {
        self.tokens.lock().refill_rate
    }

    /// Single locked critical section: refill, attempt consume, and if we
    /// cannot, compute the wait for the next token. Returning this in one
    /// call avoids taking the mutex twice per failed acquire.
    fn try_consume_or_wait(&self) -> Result<(), Duration> {
        let mut bucket = self.tokens.lock();
        bucket.refill();

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            debug!(remaining_tokens = bucket.tokens, "Token acquired");
            Ok(())
        } else {
            let tokens_needed = 1.0 - bucket.tokens;
            let seconds_to_wait = tokens_needed / bucket.refill_rate;
            Err(Duration::from_secs_f64(seconds_to_wait))
        }
    }
}

impl TokenBucket {
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();

        let tokens_to_add = elapsed * self.refill_rate;
        self.tokens = (self.tokens + tokens_to_add).min(self.capacity);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_token_bucket_limits_rate() {
        let config = RateLimitConfig {
            requests_per_second: 2,
            burst_size: 2,
        };
        let limiter = RateLimiter::new(config);

        // Should be able to acquire 2 tokens immediately (burst)
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());

        // Wait for refill
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(limiter.try_acquire());
    }

    #[tokio::test]
    async fn test_acquire_waits_for_token() {
        let config = RateLimitConfig {
            requests_per_second: 2,
            burst_size: 1,
        };
        let limiter = Arc::new(RateLimiter::new(config));

        // Acquire the initial token
        assert!(limiter.try_acquire());

        // Spawn a task that waits for a token
        let limiter_clone = Arc::clone(&limiter);
        let start = Instant::now();
        let handle = tokio::spawn(async move {
            limiter_clone.acquire().await;
        });

        // Give it a moment to reach the wait
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Wait for it to complete (should be ~500ms for 2 req/sec)
        handle.await.unwrap();
        let elapsed = start.elapsed();

        assert!(elapsed.as_millis() > 400 && elapsed.as_millis() < 700);
    }

    #[tokio::test]
    async fn test_acquire_cancellable_returns_false_when_cancelled_mid_wait() {
        use crate::utils::cancel::CancelRegistry;

        // Burst of 1, refill at 1/s — second waiter must sleep ~1s for a token.
        let config = RateLimitConfig { requests_per_second: 1, burst_size: 1 };
        let limiter = Arc::new(RateLimiter::new(config));
        assert!(limiter.try_acquire(), "first token should be available");

        let registry = CancelRegistry::new();
        let guard = registry.guard();
        let limiter_clone = Arc::clone(&limiter);

        let start = Instant::now();
        let waiter = tokio::spawn(async move { limiter_clone.acquire_cancellable(&guard).await });

        // Give the waiter a moment to enter its sleep slice, then cancel.
        tokio::time::sleep(Duration::from_millis(20)).await;
        registry.cancel_all();

        let result = tokio::time::timeout(Duration::from_millis(500), waiter)
            .await
            .expect("cancellable waiter must return within 500 ms")
            .expect("task should not panic");
        assert!(!result, "cancelled acquire must report failure");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "cancellation must preempt the long sleep, elapsed = {:?}",
            start.elapsed(),
        );
    }

    #[tokio::test]
    async fn test_acquire_cancellable_succeeds_when_token_available() {
        use crate::utils::cancel::CancelRegistry;

        let config = RateLimitConfig { requests_per_second: 10, burst_size: 5 };
        let limiter = RateLimiter::new(config);
        let registry = CancelRegistry::new();
        let guard = registry.guard();

        assert!(limiter.acquire_cancellable(&guard).await);
    }

    #[tokio::test]
    async fn drain_empties_burst_and_resumes_at_sustained_rate() {
        let config = RateLimitConfig { requests_per_second: 10, burst_size: 5 };
        let limiter = RateLimiter::new(config);
        // Full bucket: burst headroom available.
        assert!(limiter.try_acquire());
        limiter.drain();
        // Post-drain, no token is immediately available — the burst that
        // would re-trip an upstream 429 is gone.
        assert!(!limiter.try_acquire());
        // Refill resumes at the (AIMD-halved: 10 → 5/s) sustained rate —
        // one token within ~200 ms, and no burst.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire(), "only sustained-rate tokens, no burst");
    }

    /// AIMD contract: each 429 halves the refill rate (floored), and
    /// successes recover it additively, never past the configured ceiling.
    #[tokio::test]
    async fn drain_halves_rate_and_reward_recovers_to_ceiling() {
        let config = RateLimitConfig { requests_per_second: 8, burst_size: 8 };
        let limiter = RateLimiter::new(config);
        assert_eq!(limiter.current_rate(), 8.0);
        limiter.drain();
        assert_eq!(limiter.current_rate(), 4.0, "429 must halve the rate");
        limiter.drain();
        limiter.drain();
        limiter.drain();
        assert_eq!(limiter.current_rate(), 1.0, "rate floors at 1 req/s");
        for _ in 0..200 {
            limiter.reward();
        }
        assert_eq!(
            limiter.current_rate(),
            8.0,
            "sustained success recovers to the configured ceiling, never past it"
        );
    }

    #[tokio::test]
    async fn test_single_lock_per_failed_acquire() {
        // Regression guard: try_consume_or_wait should never take the lock
        // twice. We can't observe the lock directly, but we can at least
        // exercise the failure path to prove it returns a finite wait.
        let config = RateLimitConfig { requests_per_second: 1, burst_size: 1 };
        let limiter = RateLimiter::new(config);
        assert!(limiter.try_acquire());
        match limiter.try_consume_or_wait() {
            Ok(()) => panic!("bucket should be empty"),
            Err(wait) => {
                assert!(wait > Duration::from_millis(0));
                assert!(wait <= Duration::from_secs(2));
            }
        }
    }
}
