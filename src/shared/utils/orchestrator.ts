/**
 * Workflowy Insertion Orchestrator
 *
 * Coordinates multiple parallel insertion workers for heavy workloads.
 * Implements the Claude Agent SDK pattern for multi-agent orchestration.
 *
 * Architecture:
 * - Coordinator: Splits content, assigns work, tracks progress
 * - Workers: Independent insertion agents, each with own rate limiter
 * - Merger: Combines results and handles failures
 */

import { EventEmitter } from "events";
import {
  splitIntoSubtrees,
  mergeSubtreeResults,
  estimateTimeSavings,
  type Subtree,
  type SubtreeResult,
  type MergedResult,
  type SplitConfig,
} from "./subtree-parser.js";

// Re-export types for external use
export type { MergedResult } from "./subtree-parser.js";
import { RateLimiter } from "./rateLimiter.js";

// ============================================================================
// Types
// ============================================================================

export type WorkerStatus = "idle" | "working" | "completed" | "failed";

export interface WorkerState {
  id: string;
  subtreeId: string | null;
  status: WorkerStatus;
  progress: number; // 0-100
  nodesCreated: number;
  nodeIds: string[];
  error?: string;
  startTime?: number;
  endTime?: number;
}

export interface OrchestratorProgress {
  phase: "planning" | "executing" | "merging" | "completed" | "failed";
  totalSubtrees: number;
  completedSubtrees: number;
  totalNodes: number;
  createdNodes: number;
  failedNodes: number;
  workers: WorkerState[];
  elapsedMs: number;
  estimatedRemainingMs: number;
}

export interface OrchestratorConfig {
  /** Maximum concurrent workers (default: 5) */
  maxWorkers: number;
  /** Rate limit per worker: requests per second (default: 5) */
  workerRateLimit: number;
  /** Retry failed subtrees (default: true) */
  retryOnFailure: boolean;
  /** Maximum retries per subtree (default: 2) */
  maxRetries: number;
  /** Progress callback interval in ms (default: 500) */
  progressInterval: number;
  /** Subtree splitting configuration */
  splitConfig?: Partial<SplitConfig>;
}

export interface InsertionTask {
  parentId: string;
  content: string;
  position?: "top" | "bottom";
}

export type InsertionFn = (
  parentId: string,
  content: string,
  position?: "top" | "bottom"
) => Promise<Array<{ id: string; name: string }>>;

// ============================================================================
// Default Configuration
// ============================================================================

const DEFAULT_CONFIG: OrchestratorConfig = {
  maxWorkers: 5,
  workerRateLimit: 5,
  retryOnFailure: true,
  maxRetries: 2,
  progressInterval: 500,
};

// ============================================================================
// Worker Implementation
// ============================================================================

class InsertionWorker {
  readonly id: string;
  private state: WorkerState;
  private rateLimiter: RateLimiter;
  private insertFn: InsertionFn;

  constructor(
    id: string,
    insertFn: InsertionFn,
    rateLimit: number
  ) {
    this.id = id;
    this.insertFn = insertFn;
    this.rateLimiter = new RateLimiter({
      requestsPerSecond: rateLimit,
      burstSize: Math.min(rateLimit * 2, 10),
    });
    this.state = {
      id,
      subtreeId: null,
      status: "idle",
      progress: 0,
      nodesCreated: 0,
      nodeIds: [],
    };
  }

  getState(): WorkerState {
    return { ...this.state };
  }

  async processSubtree(
    subtree: Subtree,
    parentId: string,
    position?: "top" | "bottom"
  ): Promise<SubtreeResult> {
    this.state.subtreeId = subtree.id;
    this.state.status = "working";
    this.state.progress = 0;
    this.state.nodesCreated = 0;
    this.state.nodeIds = [];
    this.state.startTime = Date.now();
    this.state.error = undefined;

    try {
      // Wait for rate limiter before starting
      await this.rateLimiter.acquire();

      // Insert the subtree content
      const createdNodes = await this.insertFn(
        parentId,
        subtree.content,
        position
      );

      this.state.nodeIds = createdNodes.map((n) => n.id);
      this.state.nodesCreated = createdNodes.length;
      this.state.progress = 100;
      this.state.status = "completed";
      this.state.endTime = Date.now();

      return {
        subtreeId: subtree.id,
        success: true,
        nodeIds: this.state.nodeIds,
        durationMs: this.state.endTime - this.state.startTime,
      };
    } catch (error) {
      this.state.status = "failed";
      this.state.error = error instanceof Error ? error.message : String(error);
      this.state.endTime = Date.now();

      return {
        subtreeId: subtree.id,
        success: false,
        nodeIds: [],
        error: this.state.error,
        durationMs: this.state.endTime - (this.state.startTime || Date.now()),
      };
    }
  }

  reset(): void {
    this.state = {
      id: this.id,
      subtreeId: null,
      status: "idle",
      progress: 0,
      nodesCreated: 0,
      nodeIds: [],
    };
  }
}

// ============================================================================
// Orchestrator Implementation
// ============================================================================

export class WorkflowyInsertionOrchestrator extends EventEmitter {
  private config: OrchestratorConfig;
  private workers: InsertionWorker[] = [];
  private insertFn: InsertionFn;
  private startTime: number = 0;
  private progressTimer: ReturnType<typeof setInterval> | null = null;

  constructor(insertFn: InsertionFn, config: Partial<OrchestratorConfig> = {}) {
    super();
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.insertFn = insertFn;

    // Initialize worker pool
    for (let i = 0; i < this.config.maxWorkers; i++) {
      this.workers.push(
        new InsertionWorker(
          `worker-${i}`,
          this.insertFn,
          this.config.workerRateLimit
        )
      );
    }
  }

  /**
   * Execute a bulk insertion with parallel workers
   */
  async execute(task: InsertionTask): Promise<MergedResult> {
    this.startTime = Date.now();

    // Phase 1: Planning - split content into subtrees
    this.emitProgress("planning", [], 0, 0);

    const splitResult = splitIntoSubtrees(task.content, this.config.splitConfig);

    if (splitResult.subtrees.length === 0) {
      return {
        success: true,
        totalNodes: 0,
        createdNodes: 0,
        failedSubtrees: [],
        allNodeIds: [],
        totalDurationMs: Date.now() - this.startTime,
        errors: [],
      };
    }

    // Log planning info
    const timeSavings = estimateTimeSavings(
      splitResult.totalNodes,
      Math.min(splitResult.recommendedAgents, this.config.maxWorkers),
      this.config.workerRateLimit
    );

    this.emit("plan", {
      subtrees: splitResult.subtrees.length,
      totalNodes: splitResult.totalNodes,
      recommendedAgents: splitResult.recommendedAgents,
      estimatedSavings: timeSavings,
    });

    // Phase 2: Execute - process subtrees in parallel
    this.emitProgress("executing", splitResult.subtrees, 0, splitResult.totalNodes);

    // Start progress reporting
    this.startProgressReporting(splitResult.subtrees, splitResult.totalNodes);

    const results = await this.processSubtreesInParallel(
      splitResult.subtrees,
      task.parentId,
      task.position
    );

    // Stop progress reporting
    this.stopProgressReporting();

    // Phase 3: Handle retries for failed subtrees
    const failedResults = results.filter((r) => !r.success);
    const retriedResults: SubtreeResult[] = [];

    if (this.config.retryOnFailure && failedResults.length > 0) {
      for (const failed of failedResults) {
        const subtree = splitResult.subtrees.find((s) => s.id === failed.subtreeId);
        if (subtree) {
          for (let attempt = 0; attempt < this.config.maxRetries; attempt++) {
            const worker = this.getIdleWorker();
            if (worker) {
              const retryResult = await worker.processSubtree(
                subtree,
                task.parentId,
                task.position
              );
              if (retryResult.success) {
                retriedResults.push(retryResult);
                break;
              }
            }
          }
        }
      }
    }

    // Phase 4: Merge results
    this.emitProgress("merging", splitResult.subtrees, results.length, splitResult.totalNodes);

    // Combine original successes with retry successes
    const successfulResults = [
      ...results.filter((r) => r.success),
      ...retriedResults,
    ];

    // Get final failed subtrees (failed even after retries)
    const retriedSubtreeIds = new Set(retriedResults.map((r) => r.subtreeId));
    const finalFailedResults = results.filter(
      (r) => !r.success && !retriedSubtreeIds.has(r.subtreeId)
    );

    const mergedResult = mergeSubtreeResults([...successfulResults, ...finalFailedResults]);

    this.emitProgress(
      mergedResult.success ? "completed" : "failed",
      splitResult.subtrees,
      splitResult.subtrees.length,
      splitResult.totalNodes
    );

    // Reset workers for next use
    this.workers.forEach((w) => w.reset());

    return mergedResult;
  }

  /**
   * Process subtrees in parallel using worker pool
   */
  private async processSubtreesInParallel(
    subtrees: Subtree[],
    parentId: string,
    position?: "top" | "bottom"
  ): Promise<SubtreeResult[]> {
    const results: SubtreeResult[] = [];
    const queue = [...subtrees];

    // Process in batches matching worker count
    while (queue.length > 0) {
      const batch = queue.splice(0, this.workers.length);
      const batchPromises = batch.map((subtree, index) => {
        const worker = this.workers[index];
        return worker.processSubtree(subtree, parentId, position);
      });

      const batchResults = await Promise.allSettled(batchPromises);

      for (const result of batchResults) {
        if (result.status === "fulfilled") {
          results.push(result.value);
        } else {
          // Should not happen as worker handles errors internally
          results.push({
            subtreeId: "unknown",
            success: false,
            nodeIds: [],
            error: result.reason?.message || "Unknown error",
            durationMs: 0,
          });
        }
      }
    }

    return results;
  }

  /**
   * Get an idle worker from the pool
   */
  private getIdleWorker(): InsertionWorker | null {
    return this.workers.find((w) => w.getState().status === "idle") || null;
  }

  /**
   * Start periodic progress reporting
   */
  private startProgressReporting(subtrees: Subtree[], totalNodes: number): void {
    this.progressTimer = setInterval(() => {
      const completedSubtrees = this.workers.filter(
        (w) => w.getState().status === "completed"
      ).length;
      this.emitProgress("executing", subtrees, completedSubtrees, totalNodes);
    }, this.config.progressInterval);
  }

  /**
   * Stop progress reporting
   */
  private stopProgressReporting(): void {
    if (this.progressTimer) {
      clearInterval(this.progressTimer);
      this.progressTimer = null;
    }
  }

  /**
   * Emit progress event
   */
  private emitProgress(
    phase: OrchestratorProgress["phase"],
    subtrees: Subtree[],
    completedSubtrees: number,
    totalNodes: number
  ): void {
    const workerStates = this.workers.map((w) => w.getState());
    const createdNodes = workerStates.reduce((sum, w) => sum + w.nodesCreated, 0);
    const failedNodes = workerStates
      .filter((w) => w.status === "failed")
      .reduce((sum, w) => sum + (subtrees.find((s) => s.id === w.subtreeId)?.nodeCount || 0), 0);

    const elapsedMs = Date.now() - this.startTime;

    // Estimate remaining time based on progress
    let estimatedRemainingMs = 0;
    if (createdNodes > 0 && createdNodes < totalNodes) {
      const msPerNode = elapsedMs / createdNodes;
      estimatedRemainingMs = (totalNodes - createdNodes) * msPerNode;
    }

    const progress: OrchestratorProgress = {
      phase,
      totalSubtrees: subtrees.length,
      completedSubtrees,
      totalNodes,
      createdNodes,
      failedNodes,
      workers: workerStates,
      elapsedMs,
      estimatedRemainingMs,
    };

    this.emit("progress", progress);
  }

  /**
   * Get current worker states
   */
  getWorkerStates(): WorkerState[] {
    return this.workers.map((w) => w.getState());
  }

  /**
   * Get configuration
   */
  getConfig(): OrchestratorConfig {
    return { ...this.config };
  }
}

// ============================================================================
// Factory Function
// ============================================================================

/**
 * Create an orchestrator instance with the given insertion function
 */
export function createOrchestrator(
  insertFn: InsertionFn,
  config?: Partial<OrchestratorConfig>
): WorkflowyInsertionOrchestrator {
  return new WorkflowyInsertionOrchestrator(insertFn, config);
}

// ============================================================================
// Utility: Task File Protocol for Claude Agent SDK
// ============================================================================

/**
 * Task state for multi-agent coordination
 * Compatible with Claude Agent SDK task file protocol
 */
export interface AgentTask {
  id: string;
  type: "workflowy-insert";
  status: "pending" | "in_progress" | "completed" | "failed";
  parentId: string;
  content: string;
  position?: "top" | "bottom";
  assignedTo?: string;
  result?: {
    nodeIds: string[];
    error?: string;
    durationMs: number;
  };
  createdAt: number;
  updatedAt: number;
}

/**
 * Create a task object for the Agent SDK protocol
 */
export function createAgentTask(
  id: string,
  parentId: string,
  content: string,
  position?: "top" | "bottom"
): AgentTask {
  const now = Date.now();
  return {
    id,
    type: "workflowy-insert",
    status: "pending",
    parentId,
    content,
    position,
    createdAt: now,
    updatedAt: now,
  };
}

/**
 * Update a task with result
 */
export function updateAgentTask(
  task: AgentTask,
  result: SubtreeResult
): AgentTask {
  return {
    ...task,
    status: result.success ? "completed" : "failed",
    result: {
      nodeIds: result.nodeIds,
      error: result.error,
      durationMs: result.durationMs,
    },
    updatedAt: Date.now(),
  };
}
