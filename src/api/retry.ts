/**
 * Retry logic with exponential backoff
 */

import { RETRY_CONFIG } from "../config/environment.js";

/** Error with status code from HTTP response */
export class HttpError extends Error {
  constructor(
    message: string,
    public statusCode: number
  ) {
    super(message);
    this.name = "HttpError";
  }
}

/**
 * Calculate delay for exponential backoff
 */
function calculateDelay(attempt: number): number {
  const delay = Math.min(
    RETRY_CONFIG.baseDelay * Math.pow(2, attempt),
    RETRY_CONFIG.maxDelay
  );
  // Add jitter (0-25% of delay)
  return delay + Math.random() * delay * 0.25;
}

/**
 * Check if a status code is retryable
 */
function isRetryable(statusCode: number): boolean {
  return RETRY_CONFIG.retryableStatuses.includes(statusCode);
}

/**
 * Sleep for specified milliseconds
 */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Execute a function with retry logic
 * @param fn Function that returns a Promise
 * @param context Optional context for error messages (e.g., "POST /nodes")
 */
export async function withRetry<T>(
  fn: () => Promise<T>,
  context?: string
): Promise<T> {
  let lastError: Error | undefined;

  for (let attempt = 0; attempt < RETRY_CONFIG.maxAttempts; attempt++) {
    try {
      return await fn();
    } catch (error) {
      lastError = error instanceof Error ? error : new Error(String(error));

      // Check if this is an HTTP error we can retry
      if (error instanceof HttpError) {
        if (!isRetryable(error.statusCode)) {
          // Non-retryable error (4xx except 429), fail immediately
          throw error;
        }

        // Retryable error, log and continue
        if (attempt < RETRY_CONFIG.maxAttempts - 1) {
          const delay = calculateDelay(attempt);
          const ctx = context ? ` [${context}]` : "";
          console.error(
            `Retry ${attempt + 1}/${RETRY_CONFIG.maxAttempts}${ctx}: ${error.statusCode} - waiting ${Math.round(delay)}ms`
          );
          await sleep(delay);
        }
      } else {
        // Network error or other, retry
        if (attempt < RETRY_CONFIG.maxAttempts - 1) {
          const delay = calculateDelay(attempt);
          const ctx = context ? ` [${context}]` : "";
          console.error(
            `Retry ${attempt + 1}/${RETRY_CONFIG.maxAttempts}${ctx}: Network error - waiting ${Math.round(delay)}ms`
          );
          await sleep(delay);
        }
      }
    }
  }

  throw lastError || new Error("Retry failed");
}
