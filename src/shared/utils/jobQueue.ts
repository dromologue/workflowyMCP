/**
 * Async Job Queue for background processing of Workflowy operations
 *
 * Allows Claude to hand off large workloads to be processed in the background
 * while respecting API rate limits. Claude can check job status and retrieve
 * results when complete.
 */

import { RateLimiter, getDefaultRateLimiter } from "./rateLimiter.js";

export type JobStatus = "pending" | "processing" | "completed" | "failed" | "cancelled";

export type JobType =
  | "insert_content"
  | "insert_file"
  | "batch_operations"
  | "bulk_update"
  | "bulk_delete";

export interface JobProgress {
  /** Total items to process */
  total: number;
  /** Items completed so far */
  completed: number;
  /** Items that failed */
  failed: number;
  /** Current operation description */
  currentOperation?: string;
  /** Percentage complete (0-100) */
  percentComplete: number;
}

export interface Job<TParams = unknown, TResult = unknown> {
  /** Unique job identifier */
  id: string;
  /** Type of job */
  type: JobType;
  /** Job parameters */
  params: TParams;
  /** Current status */
  status: JobStatus;
  /** Progress information */
  progress: JobProgress;
  /** Result when completed */
  result?: TResult;
  /** Error message if failed */
  error?: string;
  /** Errors for individual items */
  itemErrors?: Array<{ index: number; error: string }>;
  /** When job was created */
  createdAt: number;
  /** When job started processing */
  startedAt?: number;
  /** When job completed */
  completedAt?: number;
  /** Optional description for display */
  description?: string;
}

export interface JobQueueConfig {
  /** Maximum concurrent jobs processing (default: 1) */
  maxConcurrentJobs: number;
  /** Rate limiter for API calls within jobs */
  rateLimiter?: RateLimiter;
  /** Delay between API calls in ms (default: 200) */
  apiCallDelay: number;
  /** Maximum jobs to keep in history (default: 100) */
  maxJobHistory: number;
  /** Time to keep completed jobs in ms (default: 30 minutes) */
  completedJobTTL: number;
}

export interface InsertContentJobParams {
  parentId: string;
  content: string;
  position?: "top" | "bottom";
}

export interface InsertFileJobParams {
  filePath: string;
  parentId: string;
  position?: "top" | "bottom";
  format?: "auto" | "markdown" | "plain";
}

export interface InsertFileJobResult {
  success: boolean;
  nodesCreated: number;
  nodeIds: string[];
  fileName: string;
  fileSize: number;
  format: string;
  errors?: string[];
}

export interface BatchOperationJobParams {
  operations: Array<{
    type: "create" | "update" | "delete" | "move" | "complete" | "uncomplete";
    params: Record<string, unknown>;
  }>;
}

export interface InsertContentJobResult {
  success: boolean;
  nodesCreated: number;
  nodeIds: string[];
  errors?: string[];
}

export interface BatchOperationJobResult {
  success: boolean;
  results: Array<{
    index: number;
    status: "success" | "failed";
    result?: unknown;
    error?: string;
  }>;
  totalSucceeded: number;
  totalFailed: number;
}

type JobExecutor<TParams, TResult> = (
  params: TParams,
  onProgress: (progress: Partial<JobProgress>) => void,
  signal: AbortSignal
) => Promise<TResult>;

const DEFAULT_CONFIG: JobQueueConfig = {
  maxConcurrentJobs: 1,
  apiCallDelay: 200,
  maxJobHistory: 100,
  completedJobTTL: 30 * 60 * 1000, // 30 minutes
};

export class JobQueue {
  private jobs: Map<string, Job> = new Map();
  private jobIdCounter = 0;
  private activeJobs = 0;
  private config: JobQueueConfig;
  private rateLimiter: RateLimiter;
  private executors: Map<JobType, JobExecutor<unknown, unknown>> = new Map();
  private abortControllers: Map<string, AbortController> = new Map();
  private cleanupInterval: ReturnType<typeof setInterval> | null = null;

  constructor(config: Partial<JobQueueConfig> = {}) {
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.rateLimiter = config.rateLimiter || getDefaultRateLimiter();
    this.startCleanupInterval();
  }

  /**
   * Register an executor for a job type
   */
  registerExecutor<TParams, TResult>(
    type: JobType,
    executor: JobExecutor<TParams, TResult>
  ): void {
    this.executors.set(type, executor as JobExecutor<unknown, unknown>);
  }

  /**
   * Submit a new job for processing
   */
  submit<TParams, TResult>(
    type: JobType,
    params: TParams,
    description?: string
  ): Job<TParams, TResult> {
    const id = `job-${Date.now()}-${++this.jobIdCounter}`;

    const job: Job<TParams, TResult> = {
      id,
      type,
      params,
      status: "pending",
      progress: {
        total: 0,
        completed: 0,
        failed: 0,
        percentComplete: 0,
      },
      createdAt: Date.now(),
      description,
    };

    this.jobs.set(id, job as Job);
    this.processQueue();

    return job;
  }

  /**
   * Get a job by ID
   */
  getJob<TParams = unknown, TResult = unknown>(id: string): Job<TParams, TResult> | undefined {
    return this.jobs.get(id) as Job<TParams, TResult> | undefined;
  }

  /**
   * Get job status summary
   */
  getJobStatus(id: string): {
    found: boolean;
    status?: JobStatus;
    progress?: JobProgress;
    error?: string;
    createdAt?: number;
    startedAt?: number;
    completedAt?: number;
    description?: string;
  } {
    const job = this.jobs.get(id);
    if (!job) {
      return { found: false };
    }

    return {
      found: true,
      status: job.status,
      progress: job.progress,
      error: job.error,
      createdAt: job.createdAt,
      startedAt: job.startedAt,
      completedAt: job.completedAt,
      description: job.description,
    };
  }

  /**
   * Get job result (only available for completed/failed jobs)
   */
  getJobResult<TResult = unknown>(id: string): {
    found: boolean;
    status?: JobStatus;
    result?: TResult;
    error?: string;
    itemErrors?: Array<{ index: number; error: string }>;
  } {
    const job = this.jobs.get(id);
    if (!job) {
      return { found: false };
    }

    return {
      found: true,
      status: job.status,
      result: job.result as TResult,
      error: job.error,
      itemErrors: job.itemErrors,
    };
  }

  /**
   * List all jobs with optional status filter
   */
  listJobs(statusFilter?: JobStatus[]): Array<{
    id: string;
    type: JobType;
    status: JobStatus;
    progress: JobProgress;
    createdAt: number;
    description?: string;
  }> {
    const jobs: Array<{
      id: string;
      type: JobType;
      status: JobStatus;
      progress: JobProgress;
      createdAt: number;
      description?: string;
    }> = [];

    for (const job of this.jobs.values()) {
      if (!statusFilter || statusFilter.includes(job.status)) {
        jobs.push({
          id: job.id,
          type: job.type,
          status: job.status,
          progress: job.progress,
          createdAt: job.createdAt,
          description: job.description,
        });
      }
    }

    // Sort by creation time, newest first
    jobs.sort((a, b) => b.createdAt - a.createdAt);
    return jobs;
  }

  /**
   * Cancel a pending or processing job
   */
  cancelJob(id: string): { success: boolean; message: string } {
    const job = this.jobs.get(id);
    if (!job) {
      return { success: false, message: `Job ${id} not found` };
    }

    if (job.status === "completed" || job.status === "failed" || job.status === "cancelled") {
      return { success: false, message: `Job ${id} is already ${job.status}` };
    }

    // Abort if processing
    const controller = this.abortControllers.get(id);
    if (controller) {
      controller.abort();
      this.abortControllers.delete(id);
    }

    job.status = "cancelled";
    job.completedAt = Date.now();
    return { success: true, message: `Job ${id} cancelled` };
  }

  /**
   * Get queue statistics
   */
  getStats(): {
    pending: number;
    processing: number;
    completed: number;
    failed: number;
    cancelled: number;
    total: number;
  } {
    const stats = {
      pending: 0,
      processing: 0,
      completed: 0,
      failed: 0,
      cancelled: 0,
      total: 0,
    };

    for (const job of this.jobs.values()) {
      stats[job.status]++;
      stats.total++;
    }

    return stats;
  }

  /**
   * Get the rate limiter for external use
   */
  getRateLimiter(): RateLimiter {
    return this.rateLimiter;
  }

  /**
   * Shutdown the queue (cancel pending jobs, stop processing)
   */
  shutdown(): void {
    // Cancel all pending jobs
    for (const job of this.jobs.values()) {
      if (job.status === "pending" || job.status === "processing") {
        this.cancelJob(job.id);
      }
    }

    // Stop cleanup interval
    if (this.cleanupInterval) {
      clearInterval(this.cleanupInterval);
      this.cleanupInterval = null;
    }
  }

  private async processQueue(): Promise<void> {
    // Don't start more jobs if at capacity
    if (this.activeJobs >= this.config.maxConcurrentJobs) {
      return;
    }

    // Find next pending job
    let nextJob: Job | undefined;
    for (const job of this.jobs.values()) {
      if (job.status === "pending") {
        nextJob = job;
        break;
      }
    }

    if (!nextJob) {
      return;
    }

    // Start processing
    this.activeJobs++;
    nextJob.status = "processing";
    nextJob.startedAt = Date.now();

    const executor = this.executors.get(nextJob.type);
    if (!executor) {
      nextJob.status = "failed";
      nextJob.error = `No executor registered for job type: ${nextJob.type}`;
      nextJob.completedAt = Date.now();
      this.activeJobs--;
      this.processQueue();
      return;
    }

    // Create abort controller for cancellation
    const abortController = new AbortController();
    this.abortControllers.set(nextJob.id, abortController);

    try {
      const result = await executor(
        nextJob.params,
        (progress) => this.updateProgress(nextJob!.id, progress),
        abortController.signal
      );

      // Re-fetch job to check if cancelled during execution (status may have changed)
      const currentJob = this.jobs.get(nextJob.id);
      if (!currentJob || currentJob.status === "cancelled") {
        return;
      }

      currentJob.status = "completed";
      currentJob.result = result;
      currentJob.completedAt = Date.now();
      currentJob.progress.percentComplete = 100;
    } catch (error) {
      // Re-fetch job to check if cancelled during execution
      const currentJob = this.jobs.get(nextJob.id);
      if (!currentJob || currentJob.status === "cancelled") {
        return;
      }

      currentJob.status = "failed";
      currentJob.error = error instanceof Error ? error.message : String(error);
      currentJob.completedAt = Date.now();
    } finally {
      this.abortControllers.delete(nextJob.id);
      this.activeJobs--;
      this.processQueue();
    }
  }

  private updateProgress(jobId: string, progress: Partial<JobProgress>): void {
    const job = this.jobs.get(jobId);
    if (!job || job.status !== "processing") {
      return;
    }

    Object.assign(job.progress, progress);

    // Calculate percentage
    if (job.progress.total > 0) {
      job.progress.percentComplete = Math.round(
        ((job.progress.completed + job.progress.failed) / job.progress.total) * 100
      );
    }
  }

  private startCleanupInterval(): void {
    // Clean up old completed jobs every 5 minutes
    this.cleanupInterval = setInterval(() => {
      const now = Date.now();
      const toDelete: string[] = [];

      for (const [id, job] of this.jobs) {
        if (
          (job.status === "completed" || job.status === "failed" || job.status === "cancelled") &&
          job.completedAt &&
          now - job.completedAt > this.config.completedJobTTL
        ) {
          toDelete.push(id);
        }
      }

      // Also enforce max history
      if (this.jobs.size > this.config.maxJobHistory) {
        const sorted = Array.from(this.jobs.entries())
          .filter(([, j]) => j.status === "completed" || j.status === "failed" || j.status === "cancelled")
          .sort((a, b) => (a[1].completedAt || 0) - (b[1].completedAt || 0));

        const excessCount = this.jobs.size - this.config.maxJobHistory;
        for (let i = 0; i < excessCount && i < sorted.length; i++) {
          toDelete.push(sorted[i][0]);
        }
      }

      for (const id of toDelete) {
        this.jobs.delete(id);
      }
    }, 5 * 60 * 1000);
  }
}

// Default job queue instance
let defaultJobQueue: JobQueue | null = null;

export function getDefaultJobQueue(): JobQueue {
  if (!defaultJobQueue) {
    defaultJobQueue = new JobQueue();
  }
  return defaultJobQueue;
}

export function resetJobQueue(): void {
  if (defaultJobQueue) {
    defaultJobQueue.shutdown();
  }
  defaultJobQueue = null;
}

/**
 * Helper to create a rate-limited executor that respects API limits
 */
export function createRateLimitedExecutor<TParams, TResult>(
  processItem: (
    params: TParams,
    index: number,
    total: number,
    onProgress: (progress: Partial<JobProgress>) => void
  ) => Promise<TResult>,
  getTotal: (params: TParams) => number
): JobExecutor<TParams, TResult> {
  return async (params, onProgress, signal) => {
    const total = getTotal(params);
    const rateLimiter = getDefaultRateLimiter();

    onProgress({ total, completed: 0, failed: 0 });

    // Check for abort before starting
    if (signal.aborted) {
      throw new Error("Job cancelled");
    }

    const result = await processItem(params, 0, total, (progress) => {
      // Check for abort during processing
      if (signal.aborted) {
        throw new Error("Job cancelled");
      }
      onProgress(progress);
    });

    return result;
  };
}
