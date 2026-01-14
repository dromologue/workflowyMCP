/**
 * Environment configuration and constants
 */

import * as dotenv from "dotenv";
import * as path from "path";
import { fileURLToPath } from "url";

// Load environment variables
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
dotenv.config({ path: path.join(__dirname, "..", "..", "..", ".env") });

/** Workflowy API configuration */
export const WORKFLOWY_API_KEY = process.env.WORKFLOWY_API_KEY;
export const WORKFLOWY_BASE_URL = "https://workflowy.com/api/v1";

/** Dropbox configuration for image hosting */
export const DROPBOX_APP_KEY = process.env.DROPBOX_APP_KEY;
export const DROPBOX_APP_SECRET = process.env.DROPBOX_APP_SECRET;
export const DROPBOX_REFRESH_TOKEN = process.env.DROPBOX_REFRESH_TOKEN;

/** Cache configuration */
export const CACHE_TTL = 30000; // 30 seconds

/** Retry configuration */
export const RETRY_CONFIG = {
  maxAttempts: 3,
  baseDelay: 1000,
  maxDelay: 10000,
  retryableStatuses: [429, 500, 502, 503, 504],
};

/** Validate required configuration */
export function validateConfig(): void {
  if (!WORKFLOWY_API_KEY) {
    console.error("Error: WORKFLOWY_API_KEY environment variable is not set");
    process.exit(1);
  }
}
