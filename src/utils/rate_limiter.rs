//! Token bucket rate limiter
//! Provides proactive throttling to prevent exceeding API limits.

use crate::config::RateLimitConfig;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::debug;

/// Smallest wait we will ever sleep before retrying, to avoid tight loops when
/// two tasks wake near-simultaneously and race for the next token.
const MIN_WAIT: Duration = Duration::from_micros(500);

pub struct RateLimiter {
    tokens: Arc<Mutex<TokenBucket>>,
}

struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        let capacity = config.burst_size as f64;

        let bucket = TokenBucket {
            tokens: capacity,
            capacity,
            refill_rate: config.requests_per_second as f64,
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

    /// Try to acquire a token without blocking; returns true on success.
    pub fn try_acquire(&self) -> bool {
        self.try_consume_or_wait().is_ok()
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
