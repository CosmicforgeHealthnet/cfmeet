/**
 * Classification of a Calling signaling failure (C4). The server answers every rejected
 * signaling credential with HTTP 401 and a stable code:
 *  - `SIGNALING_CREDENTIAL_EXPIRED`: the credential's lifetime ended: mint a fresh one
 *    (never retry the same token);
 *  - `AUTHENTICATION_FAILED`: the credential's authority is gone (endpoint revoked, the
 *    principal or minting credential no longer valid): stop using it and register the
 *    endpoint again at most once; never poll with it again.
 * Anything else is transient (network, 5xx): keep the normal bounded retry.
 */
export type SignalingFailure = "expired" | "rejected" | "transient";

type ErrorLike = { response?: { status?: number; data?: { error?: { code?: unknown } } } };

export function signalingFailure(error: unknown): SignalingFailure {
  const response = (error as ErrorLike | null | undefined)?.response;
  if (response?.status !== 401) return "transient";
  return response.data?.error?.code === "SIGNALING_CREDENTIAL_EXPIRED" ? "expired" : "rejected";
}

/** A rejected endpoint is re-registered at most once per window; after that the device
 *  stays offline until the user signs in again or turns receiving back on. */
export const REREGISTER_WINDOW_MS = 5 * 60_000;

export function mayReregister(lastReregisterAt: number | null, now: number): boolean {
  return lastReregisterAt === null || now - lastReregisterAt >= REREGISTER_WINDOW_MS;
}

/**
 * The ring offers this device must acknowledge (C1): live offers still PENDING that it has
 * not acknowledged yet. Acknowledging tells the server — and so the caller — that this
 * device received the call and is ringing.
 */
export function offersToAcknowledge(
  offers: ReadonlyArray<{ invitationId: string; status: string; actionable: boolean }>,
  acknowledged: ReadonlySet<string>,
): string[] {
  return offers
    .filter((o) => o.actionable && o.status === "PENDING" && !acknowledged.has(o.invitationId))
    .map((o) => o.invitationId);
}
