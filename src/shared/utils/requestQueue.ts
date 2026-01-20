/**
 * Request queue with controlled concurrency for batched operations
 *
 * Manages multiple API requests with:
 * - Configurable concurrency limits
 * - Request batching with delay
 * - Rate limiting integration
 * - Error handling per operation
 */

import { RateLimiter, getDefaultRateLimiter } from "./rateLimiter.js";

export type OperationType =
  | "create"
  | "update"
  | "delete"
  | "move"
  | "complete"
  | "uncomplete";

export interface QueuedOperation {
  id: string;
  type: OperationType;
  params: Record<string, unknown>;
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timestamp: number;
}

export interface QueueConfig {
  /** Maximum parallel requests (default: 3) */
  maxConcurrency: number;
  /** Wait time to collect more operations before processing (default: 50ms) */
  batchDelay: number;
  /** Maximum operations per batch (default: 20) */
  maxBatchSize: number;
  /** Optional rate limiter (uses default if not provided) */
  rateLimiter?: RateLimiter;
}

export interface BatchResult {
  operationId: string;
  status: "fulfilled" | "rejected";
  value?: unknown;
  error?: string;
}

export interface QueueStats {
  queueLength: number;
  activeRequests: number;
  totalProcessed: number;
  totalFailed: number;
}

const DEFAULT_CONFIG: QueueConfig = {
  maxConcurrency: 3,
  batchDelay: 50,
  maxBatchSize: 20,
};

type ApiRequestFn = (
  endpoint: string,
  method: string,
  body?: object
) => Promise<unknown>;

export class RequestQueue {
  private queue: QueuedOperation[] = [];
  private activeRequests = 0;
  private config: QueueConfig;
  private batchTimer: ReturnType<typeof setTimeout> | null = null;
  private rateLimiter: RateLimiter;
  private apiRequestFn: ApiRequestFn | null = null;
  private operationIdCounter = 0;
  private stats = {
    totalProcessed: 0,
    totalFailed: 0,
  };

  constructor(config: Partial<QueueConfig> = {}) {
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.rateLimiter = config.rateLimiter || getDefaultRateLimiter();
  }

  /**
   * Set the API request function (dependency injection)
   * This allows the queue to be used with different API implementations
   */
  setApiRequestFn(fn: ApiRequestFn): void {
    this.apiRequestFn = fn;
  }

  /**
   * Enqueue an operation for processing
   * Returns a promise that resolves when the operation completes
   */
  enqueue(
    operation: Omit<QueuedOperation, "resolve" | "reject" | "id" | "timestamp">
  ): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = `op-${++this.operationIdCounter}`;
      this.queue.push({
        ...operation,
        id,
        timestamp: Date.now(),
        resolve,
        reject,
      });
      this.scheduleBatch();
    });
  }

  /**
   * Enqueue multiple operations at once
   * Returns array of promises for each operation
   */
  enqueueMany(
    operations: Array<
      Omit<QueuedOperation, "resolve" | "reject" | "id" | "timestamp">
    >
  ): Promise<unknown>[] {
    return operations.map(op => this.enqueue(op));
  }

  /**
   * Get current queue statistics
   */
  getStats(): QueueStats {
    return {
      queueLength: this.queue.length,
      activeRequests: this.activeRequests,
      ...this.stats,
    };
  }

  /**
   * Clear all pending operations (rejects them)
   */
  clear(): void {
    const pending = this.queue.splice(0);
    for (const op of pending) {
      op.reject(new Error("Queue cleared"));
    }
    if (this.batchTimer) {
      clearTimeout(this.batchTimer);
      this.batchTimer = null;
    }
  }

  /**
   * Wait for all pending operations to complete
   */
  async drain(): Promise<void> {
    while (this.queue.length > 0 || this.activeRequests > 0) {
      await this.sleep(50);
    }
  }

  private scheduleBatch(): void {
    if (this.batchTimer) return;

    this.batchTimer = setTimeout(() => {
      this.batchTimer = null;
      this.processBatch();
    }, this.config.batchDelay);
  }

  private async processBatch(): Promise<void> {
    while (
      this.queue.length > 0 &&
      this.activeRequests < this.config.maxConcurrency
    ) {
      const batch = this.queue.splice(0, this.config.maxBatchSize);
      this.activeRequests++;

      // Process batch and handle completion
      this.executeBatch(batch)
        .finally(() => {
          this.activeRequests--;
          if (this.queue.length > 0) {
            this.processBatch();
          }
        })
        .catch(() => {
          // Errors handled in executeBatch
        });
    }
  }

  private async executeBatch(batch: QueuedOperation[]): Promise<void> {
    // Execute all operations in parallel with rate limiting
    const promises = batch.map(async op => {
      try {
        // Wait for rate limiter
        await this.rateLimiter.acquire();

        // Execute the operation
        const result = await this.executeOperation(op);
        this.stats.totalProcessed++;
        op.resolve(result);
      } catch (error) {
        this.stats.totalFailed++;
        op.reject(error instanceof Error ? error : new Error(String(error)));
      }
    });

    await Promise.allSettled(promises);
  }

  private async executeOperation(op: QueuedOperation): Promise<unknown> {
    if (!this.apiRequestFn) {
      throw new Error(
        "API request function not set. Call setApiRequestFn first."
      );
    }

    switch (op.type) {
      case "create":
        return this.apiRequestFn("/nodes", "POST", op.params as object);

      case "update": {
        const { node_id, ...updateParams } = op.params;
        return this.apiRequestFn(
          `/nodes/${node_id}`,
          "POST",
          updateParams as object
        );
      }

      case "delete": {
        const nodeId = op.params.node_id;
        return this.apiRequestFn(`/nodes/${nodeId}`, "DELETE");
      }

      case "move": {
        const { node_id: moveNodeId, ...moveParams } = op.params;
        return this.apiRequestFn(
          `/nodes/${moveNodeId}`,
          "POST",
          moveParams as object
        );
      }

      case "complete": {
        const completeNodeId = op.params.node_id;
        return this.apiRequestFn(`/nodes/${completeNodeId}/complete`, "POST");
      }

      case "uncomplete": {
        const uncompleteNodeId = op.params.node_id;
        return this.apiRequestFn(
          `/nodes/${uncompleteNodeId}/uncomplete`,
          "POST"
        );
      }

      default:
        throw new Error(`Unknown operation type: ${op.type}`);
    }
  }

  private sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}

/**
 * Default request queue instance
 */
let defaultRequestQueue: RequestQueue | null = null;

export function getDefaultRequestQueue(): RequestQueue {
  if (!defaultRequestQueue) {
    defaultRequestQueue = new RequestQueue();
  }
  return defaultRequestQueue;
}

/**
 * Initialize the default request queue with an API request function
 */
export function initializeRequestQueue(apiRequestFn: ApiRequestFn): void {
  const queue = getDefaultRequestQueue();
  queue.setApiRequestFn(apiRequestFn);
}

/**
 * Reset the default request queue (useful for testing)
 */
export function resetRequestQueue(): void {
  if (defaultRequestQueue) {
    defaultRequestQueue.clear();
  }
  defaultRequestQueue = null;
}
