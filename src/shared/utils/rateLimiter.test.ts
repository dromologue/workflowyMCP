import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  RateLimiter,
  getDefaultRateLimiter,
  resetDefaultRateLimiter,
} from "./rateLimiter.js";

describe("RateLimiter", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetDefaultRateLimiter();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe("constructor", () => {
    it("creates a rate limiter with specified config", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 10,
        burstSize: 20,
      });
      expect(limiter.getAvailableTokens()).toBe(20);
    });
  });

  describe("tryAcquire", () => {
    it("acquires token when available", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 3,
      });

      expect(limiter.tryAcquire()).toBe(true);
      expect(limiter.getAvailableTokens()).toBe(2);
    });

    it("returns false when no tokens available", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 2,
      });

      expect(limiter.tryAcquire()).toBe(true);
      expect(limiter.tryAcquire()).toBe(true);
      expect(limiter.tryAcquire()).toBe(false);
    });

    it("refills tokens over time", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 5,
      });

      // Exhaust all tokens
      for (let i = 0; i < 5; i++) {
        limiter.tryAcquire();
      }
      expect(limiter.tryAcquire()).toBe(false);

      // Advance time by 200ms (should add 1 token at 5/sec)
      vi.advanceTimersByTime(200);
      expect(limiter.tryAcquire()).toBe(true);
    });
  });

  describe("acquire", () => {
    it("resolves immediately when token available", async () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 5,
      });

      const startTime = Date.now();
      await limiter.acquire();
      const elapsed = Date.now() - startTime;

      expect(elapsed).toBe(0);
    });

    it("waits for token when none available", async () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5, // 1 token per 200ms
        burstSize: 1,
      });

      // Use the only token
      await limiter.acquire();

      // Start acquiring another token
      const acquirePromise = limiter.acquire();

      // Should need to wait ~200ms for next token
      expect(limiter.getWaitTime()).toBeGreaterThan(0);

      // Advance time
      vi.advanceTimersByTime(200);

      await acquirePromise;
    });
  });

  describe("getWaitTime", () => {
    it("returns 0 when tokens available", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 5,
      });

      expect(limiter.getWaitTime()).toBe(0);
    });

    it("returns positive value when no tokens", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 1,
      });

      limiter.tryAcquire();
      expect(limiter.getWaitTime()).toBeGreaterThan(0);
    });
  });

  describe("getAvailableTokens", () => {
    it("returns initial burst size", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 10,
      });

      expect(limiter.getAvailableTokens()).toBe(10);
    });

    it("decreases after acquire", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 10,
      });

      limiter.tryAcquire();
      expect(limiter.getAvailableTokens()).toBe(9);
    });

    it("does not exceed burst size", () => {
      const limiter = new RateLimiter({
        requestsPerSecond: 5,
        burstSize: 5,
      });

      // Wait a long time
      vi.advanceTimersByTime(10000);

      expect(limiter.getAvailableTokens()).toBe(5);
    });
  });

  describe("getDefaultRateLimiter", () => {
    it("returns a singleton instance", () => {
      const limiter1 = getDefaultRateLimiter();
      const limiter2 = getDefaultRateLimiter();

      expect(limiter1).toBe(limiter2);
    });

    it("creates limiter with default config", () => {
      const limiter = getDefaultRateLimiter();

      // Default: 5 req/sec, burst of 10
      expect(limiter.getAvailableTokens()).toBe(10);
    });
  });

  describe("resetDefaultRateLimiter", () => {
    it("resets the singleton", () => {
      const limiter1 = getDefaultRateLimiter();
      limiter1.tryAcquire();
      limiter1.tryAcquire();

      resetDefaultRateLimiter();

      const limiter2 = getDefaultRateLimiter();
      expect(limiter2).not.toBe(limiter1);
      expect(limiter2.getAvailableTokens()).toBe(10);
    });
  });
});
