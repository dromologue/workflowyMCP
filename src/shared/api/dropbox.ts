/**
 * Dropbox API client for image hosting
 */

import {
  DROPBOX_APP_KEY,
  DROPBOX_APP_SECRET,
  DROPBOX_REFRESH_TOKEN,
} from "../config/environment.js";
import type { DropboxTokenResponse, DropboxUploadResult } from "../types/index.js";

/** Cache for Dropbox access token */
let dropboxAccessToken: string | null = null;
let dropboxTokenExpiry: number = 0;

/**
 * Check if Dropbox is configured
 */
export function isDropboxConfigured(): boolean {
  return !!(DROPBOX_APP_KEY && DROPBOX_APP_SECRET && DROPBOX_REFRESH_TOKEN);
}

/**
 * Get a valid Dropbox access token (refreshes if needed)
 */
export async function getDropboxAccessToken(): Promise<string | null> {
  if (!isDropboxConfigured()) {
    return null;
  }

  // Return cached token if still valid (with 5 min buffer)
  if (dropboxAccessToken && Date.now() < dropboxTokenExpiry - 300000) {
    return dropboxAccessToken;
  }

  try {
    const response = await fetch("https://api.dropbox.com/oauth2/token", {
      method: "POST",
      headers: {
        "Content-Type": "application/x-www-form-urlencoded",
      },
      body: new URLSearchParams({
        grant_type: "refresh_token",
        refresh_token: DROPBOX_REFRESH_TOKEN!,
        client_id: DROPBOX_APP_KEY!,
        client_secret: DROPBOX_APP_SECRET!,
      }),
    });

    if (!response.ok) {
      return null;
    }

    const data = (await response.json()) as DropboxTokenResponse;
    dropboxAccessToken = data.access_token;
    dropboxTokenExpiry = Date.now() + data.expires_in * 1000;
    return dropboxAccessToken;
  } catch {
    return null;
  }
}

/**
 * Upload a file to a Dropbox path and get a shareable link.
 * Accepts a Buffer or string content.
 */
export async function uploadToDropboxPath(
  content: Buffer | string,
  dropboxPath: string
): Promise<DropboxUploadResult> {
  const accessToken = await getDropboxAccessToken();

  if (!accessToken) {
    return {
      success: false,
      error:
        "Dropbox not configured. Set DROPBOX_APP_KEY, DROPBOX_APP_SECRET, and DROPBOX_REFRESH_TOKEN in .env",
    };
  }

  try {
    const uploadPath = dropboxPath;
    const body = typeof content === "string" ? new TextEncoder().encode(content) : new Uint8Array(content);

    // Upload file to Dropbox
    const uploadResponse = await fetch(
      "https://content.dropboxapi.com/2/files/upload",
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${accessToken}`,
          "Content-Type": "application/octet-stream",
          "Dropbox-API-Arg": JSON.stringify({
            path: uploadPath,
            mode: "overwrite",
            autorename: false,
          }),
        },
        body,
      }
    );

    if (!uploadResponse.ok) {
      const errorText = await uploadResponse.text();
      return { success: false, error: `Dropbox upload failed: ${errorText}` };
    }

    // Create a shared link
    const shareResponse = await fetch(
      "https://api.dropboxapi.com/2/sharing/create_shared_link_with_settings",
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${accessToken}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          path: uploadPath,
          settings: { requested_visibility: "public" },
        }),
      }
    );

    let shareUrl: string;

    if (shareResponse.ok) {
      const shareData = (await shareResponse.json()) as { url: string };
      shareUrl = shareData.url;
    } else {
      // Link might already exist, try to get it
      const getResponse = await fetch(
        "https://api.dropboxapi.com/2/sharing/list_shared_links",
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${accessToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ path: uploadPath, direct_only: true }),
        }
      );

      if (!getResponse.ok) {
        return { success: false, error: "Failed to get shareable link" };
      }

      const linksData = (await getResponse.json()) as {
        links: Array<{ url: string }>;
      };
      if (linksData.links.length === 0) {
        return { success: false, error: "No shareable link found" };
      }
      shareUrl = linksData.links[0].url;
    }

    return { success: true, url: shareUrl };
  } catch (err) {
    return {
      success: false,
      error: `Dropbox error: ${err instanceof Error ? err.message : String(err)}`,
    };
  }
}

/**
 * Upload an image to Dropbox and get a shareable link
 */
export async function uploadToDropbox(
  imageBuffer: Buffer,
  filename: string
): Promise<DropboxUploadResult> {
  return uploadToDropboxPath(imageBuffer, `/workflowy/conceptMaps/${filename}`);
}
