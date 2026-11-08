/**
 * Media reconnect state machine (CAL-015).
 *
 * LiveKit retries a dropped connection by itself first (`provider-reconnecting`). When it
 * gives up (disconnect that the user did not ask for) while the Call is still live, the web
 * client requests a FRESH session media credential and rejoins, with bounded exponential
 * backoff, and surfaces every step. The backend admits a same-generation rejoin of the same
 * session within its participation grace (60 s), so the attempt budget stays inside it.
 *
 * Pure reducer, dependency-free (node tests import it with --experimental-strip-types).
 */

export type MediaStatus =
  | "idle"
  | "connecting"
  | "connected"
  | "provider-reconnecting"
  | "reconnecting"
  | "failed"
  | "left";

export interface MediaState {
  status: MediaStatus;
  /** Rejoin attempts made since the last successful connection. */
  attempt: number;
  /** Delay before the next rejoin attempt (only while `reconnecting`). */
  retryInMs: number | null;
  /** Safe, user-facing reason of the last failure. */
  reason: string | null;
}

export type MediaEvent =
  | { type: "JOIN" }
  | { type: "CONNECTED" }
  | { type: "PROVIDER_RECONNECTING" }
  | { type: "PROVIDER_RECONNECTED" }
  | { type: "DISCONNECTED"; userInitiated: boolean; callLive: boolean; reason?: string }
  | { type: "REJOIN_STARTED" }
  | { type: "REJOIN_FAILED"; retryable: boolean; reason?: string }
  | { type: "RETRY_NOW" }
  | { type: "LEAVE" }
  | { type: "CALL_ENDED" };

export interface ReconnectPolicy {
  maxAttempts: number;
  baseDelayMs: number;
  maxDelayMs: number;
}

export const DEFAULT_RECONNECT_POLICY: ReconnectPolicy = { maxAttempts: 5, baseDelayMs: 1_000, maxDelayMs: 15_000 };

export const INITIAL_MEDIA_STATE: MediaState = { status: "idle", attempt: 0, retryInMs: null, reason: null };

export function backoffDelay(attempt: number, policy: ReconnectPolicy = DEFAULT_RECONNECT_POLICY): number {
  return Math.min(policy.maxDelayMs, policy.baseDelayMs * 2 ** Math.max(0, attempt));
}

function scheduleOrFail(state: MediaState, reason: string | null, policy: ReconnectPolicy): MediaState {
  if (state.attempt >= policy.maxAttempts) {
    return { status: "failed", attempt: state.attempt, retryInMs: null, reason: reason ?? "The call audio could not be restored." };
  }
  return { status: "reconnecting", attempt: state.attempt, retryInMs: backoffDelay(state.attempt, policy), reason };
}

export function mediaReducer(state: MediaState, event: MediaEvent, policy: ReconnectPolicy = DEFAULT_RECONNECT_POLICY): MediaState {
  switch (event.type) {
    case "JOIN":
      return { status: "connecting", attempt: 0, retryInMs: null, reason: null };
    case "CONNECTED":
    case "PROVIDER_RECONNECTED":
      if (state.status === "left") return state;
      return { status: "connected", attempt: 0, retryInMs: null, reason: null };
    case "PROVIDER_RECONNECTING":
      if (state.status !== "connected") return state;
      return { ...state, status: "provider-reconnecting" };
    case "DISCONNECTED":
      if (state.status === "left" || state.status === "idle") return state;
      if (event.userInitiated) return { status: "left", attempt: 0, retryInMs: null, reason: null };
      if (!event.callLive) return { status: "left", attempt: 0, retryInMs: null, reason: null };
      return scheduleOrFail(state, event.reason ?? "Connection lost. Reconnecting…", policy);
    case "REJOIN_STARTED":
      if (state.status !== "reconnecting") return state;
      return { status: "connecting", attempt: state.attempt + 1, retryInMs: null, reason: state.reason };
    case "REJOIN_FAILED":
      if (state.status === "left") return state;
      if (!event.retryable) {
        return { status: "failed", attempt: state.attempt, retryInMs: null, reason: event.reason ?? "The call audio could not be restored." };
      }
      return scheduleOrFail(state, event.reason ?? null, policy);
    case "RETRY_NOW":
      // A manual retry after the budget ran out starts a fresh budget.
      if (state.status !== "failed" && state.status !== "reconnecting") return state;
      return { status: "reconnecting", attempt: state.status === "failed" ? 0 : state.attempt, retryInMs: 0, reason: state.reason };
    case "LEAVE":
    case "CALL_ENDED":
      return { status: "left", attempt: 0, retryInMs: null, reason: null };
  }
}

/** HTTP statuses of a media-credential request that make a retry pointless. */
export function isRetryableCredentialFailure(status: number | undefined): boolean {
  if (status === undefined) return true; // network error / timeout
  if (status === 401 || status === 403 || status === 404 || status === 409 || status === 410) return false;
  return status === 408 || status === 429 || status >= 500;
}

export function mediaStatusLabel(state: MediaState): string {
  switch (state.status) {
    case "idle": return "Not connected";
    case "connecting": return state.attempt > 0 ? `Reconnecting (attempt ${state.attempt})…` : "Connecting call audio…";
    case "connected": return "Call audio connected";
    case "provider-reconnecting": return "Network interrupted. Reconnecting…";
    case "reconnecting": return state.retryInMs ? `Connection lost. Retrying in ${Math.ceil(state.retryInMs / 1000)} s…` : "Connection lost. Retrying…";
    case "failed": return state.reason ?? "The call audio could not be restored.";
    case "left": return "Disconnected";
  }
}
