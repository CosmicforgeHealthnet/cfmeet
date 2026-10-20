/**
 * Part V §104: authenticated CF Meet login -> short-lived, device-bound Messaging
 * access token, via the *Meeting Service* identity bridge. No platform/service
 * credentials enter the browser, and a login bearer is never sent to Messaging.
 *
 * Call this from the signed-in web app with a registered MLS device ID and a
 * getLoginToken callback; pass the resulting provider to MessagingRealtimeTransport.
 * This module does NOT create/hold cryptographic keys or enable production E2EE.
 */
import type { MessagingAccessToken } from "./messaging-realtime";

export type MessagingSessionOptions = {
  /** The actual CF Meet Meeting Service API-v1 base, e.g. https://api.cfmeet.com/api/v1 */
  meetingApiV1Url: string;
  /** Verified CF Meet login bearer, not a Platform Access service credential. */
  getLoginToken: () => string | null | Promise<string | null>;
  /** Server-registered device whose private MLS keys remain client-side. */
  deviceId: string;
  fetchImpl?: typeof fetch;
};

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export class MessagingSessionError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MessagingSessionError";
  }
}

function tokenEndpoint(base: string): URL {
  let url: URL;
  try {
    url = new URL(base);
  } catch {
    throw new MessagingSessionError("Invalid CF Meet API origin");
  }
  const localhost = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if ((url.protocol !== "https:" && !(localhost && url.protocol === "http:")) ||
      url.username || url.password || url.search || url.hash ||
      !/^\/api\/v1\/?$/.test(url.pathname)) {
    throw new MessagingSessionError("Expected a secure CF Meet /api/v1 base URL");
  }
  url.pathname = "/api/v2/messaging/client-tokens";
  return url;
}

/**
 * Uses the Meeting Service's verified-login bridge. The bridge itself resolves
 * the principal and calls Messaging over service-authenticated internal HTTP.
 * Tokens are returned only in memory to the caller; never persist or log them.
 */
export function messagingTokenProvider(options: MessagingSessionOptions): () => Promise<MessagingAccessToken> {
  const endpoint = tokenEndpoint(options.meetingApiV1Url);
  if (!UUID.test(options.deviceId)) throw new MessagingSessionError("Invalid registered device ID");
  return async () => {
    const bearer = await options.getLoginToken();
    if (!bearer || typeof bearer !== "string" || bearer.length > 8192) {
      throw new MessagingSessionError("CF Meet authentication required");
    }
    const request = options.fetchImpl ?? fetch;
    let response: Response;
    try {
      response = await request(endpoint, {
        method: "POST",
        credentials: "omit",
        redirect: "error",
        cache: "no-store",
        headers: {
          Authorization: "Bearer " + bearer,
          "Content-Type": "application/json",
          Accept: "application/json",
        },
        body: JSON.stringify({ deviceId: options.deviceId }),
      });
    } catch {
      throw new MessagingSessionError("CF Meet Messaging session exchange unavailable");
    }
    if (!response.ok) {
      throw new MessagingSessionError(
        [401, 403].includes(response.status)
          ? "CF Meet Messaging session not authorized"
          : "CF Meet Messaging session exchange failed",
      );
    }
    let body: unknown;
    try {
      body = await response.json();
    } catch {
      throw new MessagingSessionError("Invalid Messaging session response");
    }
    if (!body || typeof body !== "object" || Array.isArray(body)) {
      throw new MessagingSessionError("Invalid Messaging session response");
    }
    const data = body as Record<string, unknown>;
    if (data.tokenType !== "CF-Client-Token" ||
        typeof data.token !== "string" || !data.token || data.token.length > 2048 ||
        typeof data.deviceId !== "string" ||
        data.deviceId.toLowerCase() !== options.deviceId.toLowerCase() ||
        typeof data.sessionId !== "string" || !UUID.test(data.sessionId) ||
        typeof data.expiresAt !== "string" || !(Date.parse(data.expiresAt) > Date.now())) {
      throw new MessagingSessionError("Invalid or expired device-bound Messaging session");
    }
    return {
      token: data.token,
      tokenType: "CF-Client-Token",
      expiresAt: data.expiresAt,
    };
  };
}
