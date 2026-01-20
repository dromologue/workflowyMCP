/**
 * Token bucket rate limiter for controlling API request rates
 *
 * Prevents overwhelming the Workflowy API with too many concurrent requests
 * by implementing a token bucket algorithm with configurable burst capacity.
 */

export interface RateLimiterConfig {
  /** Maximum requests per second */
  requestsPerSecond: number;
  /** Maximum burst size (tokens available at once) */
  burstSize: number;
}

export class RateLimiter {
  private tokens: number;
  private lastRefill: number;
  private readonly maxTokens: number;
  private readonly refillRate: number; // tokens per millisecond

  constructor(config: RateLimiterConfig) {
    this.maxTokens = config.burstSize;
    this.tokens = config.burstSize;
    this.refillRate = config.requestsPerSecond / 1000;
    this.lastRefill = Date.now();
  }

  /**
   * Acquire a token, waiting if necessary
   * Returns when a token is available
   */
  async acquire(): Promise<void> {
    this.refill();

    if (this.tokens >= 1) {
      this.tokens -= 1;
      return;
    }

    // Calculate wait time for next token
    const tokensNeeded = 1 - this.tokens;
    const waitTime = Math.ceil(tokensNeeded / this.refillRate);

    await this.sleep(waitTime);
    this.refill();
    this.tokens -= 1;
  }

  /**
   * Try to acquire a token without waiting
   * Returns true if token was acquired, false otherwise
   */
  tryAcquire(): boolean {
    this.refill();

    if (this.tokens >= 1) {
      this.tokens -= 1;
      return true;
    }

    return false;
  }

  /**
   * Get current available tokens (for monitoring)
   */
  getAvailableTokens(): number {
    this.refill();
    return this.tokens;
  }

  /**
   * Get estimated wait time in milliseconds for next token
   */
  getWaitTime(): number {
    this.refill();
    if (this.tokens >= 1) return 0;

    const tokensNeeded = 1 - this.tokens;
    return Math.ceil(tokensNeeded / this.refillRate);
  }

  private refill(): void {
    const now = Date.now();
    const elapsed = now - this.lastRefill;
    const newTokens = elapsed * this.refillRate;

    this.tokens = Math.min(this.maxTokens, this.tokens + newTokens);
    this.lastRefill = now;
  }

  private sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}

/**
 * Default rate limiter instance configured for Workflowy API
 * 5 requests/second with burst of 10
 */
let defaultRateLimiter: RateLimiter | null = null;

export function getDefaultRateLimiter(): RateLimiter {
  if (!defaultRateLimiter) {
    defaultRateLimiter = new RateLimiter({
      requestsPerSecond: 5,
      burstSize: 10,
    });
  }
  return defaultRateLimiter;
}

/**
 * Reset the default rate limiter (useful for testing)
 */
export function resetDefaultRateLimiter(): void {
  defaultRateLimiter = null;
}
