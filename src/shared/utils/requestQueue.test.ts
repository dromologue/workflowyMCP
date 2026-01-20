import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";
import {
  RequestQueue,
  getDefaultRequestQueue,
  resetRequestQueue,
  initializeRequestQueue,
} from "./requestQueue.js";
import { RateLimiter } from "./rateLimiter.js";

describe("RequestQueue", () => {
  let mockApiRequest: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.useFakeTimers();
    resetRequestQueue();
    mockApiRequest = vi.fn();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe("constructor", () => {
    it("creates queue with default config", () => {
      const queue = new RequestQueue();
      const stats = queue.getStats();

      expect(stats.queueLength).toBe(0);
      expect(stats.activeRequests).toBe(0);
      expect(stats.totalProcessed).toBe(0);
      expect(stats.totalFailed).toBe(0);
    });

    it("accepts custom config", () => {
      const queue = new RequestQueue({
        maxConcurrency: 5,
        batchDelay: 100,
        maxBatchSize: 10,
      });

      expect(queue).toBeDefined();
    });
  });

  describe("setApiRequestFn", () => {
    it("sets the API request function", async () => {
      const queue = new RequestQueue({
        rateLimiter: new RateLimiter({ requestsPerSecond: 100, burstSize: 100 }),
      });
      queue.setApiRequestFn(mockApiRequest);

      mockApiRequest.mockResolvedValueOnce({ id: "123" });

      const promise = queue.enqueue({ type: "create", params: { name: "Test" } });
      vi.advanceTimersByTime(100);
      await promise;

      expect(mockApiRequest).toHaveBeenCalledWith("/nodes", "POST", { name: "Test" });
    });
  });

  describe("enqueue", () => {
    it("queues an operation and returns a promise", () => {
      const queue = new RequestQueue();
      queue.setApiRequestFn(mockApiRequest);

      const promise = queue.enqueue({ type: "create", params: { name: "Test" } });
      expect(promise).toBeInstanceOf(Promise);
    });

    it("resolves with result on success", async () => {
      const queue = new RequestQueue({
        rateLimiter: new RateLimiter({ requestsPerSecond: 100, burstSize: 100 }),
      });
      queue.setApiRequestFn(mockApiRequest);
      mockApiRequest.mockResolvedValueOnce({ id: "123", name: "Test" });

      const promise = queue.enqueue({ type: "create", params: { name: "Test" } });
      vi.advanceTimersByTime(100);

      const result = await promise;
      expect(result).toEqual({ id: "123", name: "Test" });
    });

    it("rejects on failure", async () => {
      const queue = new RequestQueue({
        rateLimiter: new RateLimiter({ requestsPerSecond: 100, burstSize: 100 }),
      });
      queue.setApiRequestFn(mockApiRequest);
      mockApiRequest.mockRejectedValueOnce(new Error("API Error"));

      const promise = queue.enqueue({ type: "create", params: { name: "Test" } });
      vi.advanceTimersByTime(100);

      await expect(promise).rejects.toThrow("API Error");
    });

    it("rejects if API function not set", async () => {
      const queue = new RequestQueue({
        rateLimiter: new RateLimiter({ requestsPerSecond: 100, burstSize: 100 }),
      });
      // Don't set API function

      const promise = queue.enqueue({ type: "create", params: { name: "Test" } });
      vi.advanceTimersByTime(100);

      await expect(promise).rejects.toThrow("API request function not set");
    });
  });

  describe("operation types", () => {
    let queue: RequestQueue;

    beforeEach(() => {
      queue = new RequestQueue({
        rateLimiter: new RateLimiter({ requestsPerSecond: 100, burstSize: 100 }),
      });
      queue.setApiRequestFn(mockApiRequest);
      mockApiRequest.mockResolvedValue({ success: true });
    });

    it("handles create operation", async () => {
      const promise = queue.enqueue({
        type: "create",
        params: { name: "Test", parent_id: "parent1" },
      });
      vi.advanceTimersByTime(100);
      await promise;

      expect(mockApiRequest).toHaveBeenCalledWith("/nodes", "POST", {
        name: "Test",
        parent_id: "parent1",
      });
    });

    it("handles update operation", async () => {
      const promise = queue.enqueue({
        type: "update",
        params: { node_id: "node1", name: "Updated" },
      });
      vi.advanceTimersByTime(100);
      await promise;

      expect(mockApiRequest).toHaveBeenCalledWith("/nodes/node1", "POST", {
        name: "Updated",
      });
    });

    it("handles delete operation", async () => {
      const promise = queue.enqueue({
        type: "delete",
        params: { node_id: "node1" },
      });
      vi.advanceTimersByTime(100);
      await promise;

      expect(mockApiRequest).toHaveBeenCalledWith("/nodes/node1", "DELETE");
    });

    it("handles move operation", async () => {
      const promise = queue.enqueue({
        type: "move",
        params: { node_id: "node1", parent_id: "parent2" },
      });
      vi.advanceTimersByTime(100);
      await promise;

      expect(mockApiRequest).toHaveBeenCalledWith("/nodes/node1", "POST", {
        parent_id: "parent2",
      });
    });

    it("handles complete operation", async () => {
      const promise = queue.enqueue({
        type: "complete",
        params: { node_id: "node1" },
      });
      vi.advanceTimersByTime(100);
      await promise;

      expect(mockApiRequest).toHaveBeenCalledWith("/nodes/node1/complete", "POST");
    });

    it("handles uncomplete operation", async () => {
      const promise = queue.enqueue({
        type: "uncomplete",
        params: { node_id: "node1" },
      });
      vi.advanceTimersByTime(100);
      await promise;

      expect(mockApiRequest).toHaveBeenCalledWith("/nodes/node1/uncomplete", "POST");
    });
  });

  describe("enqueueMany", () => {
    it("enqueues multiple operations", async () => {
      const queue = new RequestQueue({
        rateLimiter: new RateLimiter({ requestsPerSecond: 100, burstSize: 100 }),
      });
      queue.setApiRequestFn(mockApiRequest);
      mockApiRequest.mockResolvedValue({ id: "123" });

      const promises = queue.enqueueMany([
        { type: "create", params: { name: "Test 1" } },
        { type: "create", params: { name: "Test 2" } },
        { type: "create", params: { name: "Test 3" } },
      ]);

      expect(promises).toHaveLength(3);

      vi.advanceTimersByTime(100);
      await Promise.all(promises);

      expect(mockApiRequest).toHaveBeenCalledTimes(3);
    });
  });

  describe("getStats", () => {
    it("returns correct statistics", async () => {
      const queue = new RequestQueue({
        rateLimiter: new RateLimiter({ requestsPerSecond: 100, burstSize: 100 }),
      });
      queue.setApiRequestFn(mockApiRequest);
      mockApiRequest.mockResolvedValue({ id: "123" });

      const promise = queue.enqueue({ type: "create", params: { name: "Test" } });
      vi.advanceTimersByTime(100);
      await promise;

      const stats = queue.getStats();
      expect(stats.totalProcessed).toBe(1);
      expect(stats.totalFailed).toBe(0);
    });

    it("tracks failed operations", async () => {
      const queue = new RequestQueue({
        rateLimiter: new RateLimiter({ requestsPerSecond: 100, burstSize: 100 }),
      });
      queue.setApiRequestFn(mockApiRequest);
      mockApiRequest.mockRejectedValueOnce(new Error("Fail"));

      const promise = queue.enqueue({ type: "create", params: { name: "Test" } });
      vi.advanceTimersByTime(100);

      try {
        await promise;
      } catch {
        // Expected
      }

      const stats = queue.getStats();
      expect(stats.totalFailed).toBe(1);
    });
  });

  describe("clear", () => {
    it("clears pending operations and rejects their promises", async () => {
      const queue = new RequestQueue();
      queue.setApiRequestFn(mockApiRequest);

      // Queue some operations but don't process them yet
      const promise1 = queue.enqueue({ type: "create", params: { name: "Test 1" } });
      const promise2 = queue.enqueue({ type: "create", params: { name: "Test 2" } });

      // Clear should reject pending
      queue.clear();

      const stats = queue.getStats();
      expect(stats.queueLength).toBe(0);

      // Verify promises were rejected
      await expect(promise1).rejects.toThrow("Queue cleared");
      await expect(promise2).rejects.toThrow("Queue cleared");
    });
  });

  describe("getDefaultRequestQueue", () => {
    it("returns a singleton", () => {
      const queue1 = getDefaultRequestQueue();
      const queue2 = getDefaultRequestQueue();

      expect(queue1).toBe(queue2);
    });
  });

  describe("initializeRequestQueue", () => {
    it("sets API function on default queue", async () => {
      initializeRequestQueue(mockApiRequest);
      mockApiRequest.mockResolvedValueOnce({ id: "123" });

      const queue = getDefaultRequestQueue();
      const promise = queue.enqueue({ type: "create", params: { name: "Test" } });

      vi.advanceTimersByTime(100);
      await promise;

      expect(mockApiRequest).toHaveBeenCalled();
    });
  });

  describe("resetRequestQueue", () => {
    it("clears and resets the singleton", () => {
      const queue1 = getDefaultRequestQueue();
      resetRequestQueue();
      const queue2 = getDefaultRequestQueue();

      expect(queue1).not.toBe(queue2);
    });
  });
});
