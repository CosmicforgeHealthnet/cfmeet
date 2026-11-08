/**
 * Communications Hub error catalogue (communications-unified-handoff.md §7). Pure: node tests
 * import it directly.
 */
export class CommunicationsApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    public readonly detail: string | null,
    public readonly retryAfterSeconds: number | null,
  ) {
    super(detail ?? code);
    this.name = "CommunicationsApiError";
  }
}

/** A user-facing sentence for a Hub (or relayed Messaging / Calling) error. */
export function communicationsErrorMessage(error: unknown): string {
  const code = error && typeof error === "object" && "code" in error ? String((error as { code: unknown }).code) : "";
  const status = error && typeof error === "object" && "status" in error ? Number((error as { status: unknown }).status) : 0;
  switch (code) {
    case "AUTHENTICATION_REQUIRED":
      return "Your session has expired. Sign in again to continue.";
    case "COMMUNICATIONS_NOT_ACTIVATED":
      return "Personal communications are not activated for this account yet.";
    case "ACTION_NOT_ALLOWED":
    case "PERMISSION_DENIED":
      return "This action is not allowed (privacy settings, blocking or your account state).";
    case "NOT_FOUND":
      // Neutral by design: expired, revoked, hidden and blocked look the same.
      return "Not found, or no longer available.";
    case "VERSION_CONFLICT":
    case "IDEMPOTENCY_CONFLICT":
      return "This changed elsewhere. Refresh and try again.";
    case "ALREADY_EXISTS":
      return "This already exists.";
    case "REQUEST_PENDING":
      return "A contact request is already pending.";
    case "POLICY_VALIDATION_FAILED":
      return "A limit was reached. Try again later or remove something first.";
    case "RATE_LIMITED":
      return "Too many requests. Wait a moment and try again.";
    case "VALIDATION_ERROR":
      return "The request is invalid. Check the details and try again.";
    case "DEPENDENCY_NOT_CONFIGURED":
      return "This feature is not configured in this environment.";
    case "COMMUNICATIONS_UNAVAILABLE":
      return "Communications are temporarily unavailable. Try again shortly.";
    case "E2EE_NOT_AVAILABLE":
    case "E2EE_DEVICES_DISABLED":
      return "End-to-end encrypted messaging is not available in this environment.";
    case "TARGET_UNAVAILABLE":
      return "This person cannot be reached right now.";
    case "NETWORK_UNAVAILABLE":
      return "You appear to be offline. Check your connection.";
    default:
      if (status === 401) return "Your session has expired. Sign in again to continue.";
      if (status >= 500) return "Communications are temporarily unavailable. Try again shortly.";
      return error instanceof Error && error.message ? error.message : "The request failed. Please try again.";
  }
}

/** Whether a capability answer turns an area off (default deny while unknown is the caller's choice). */
export function isUnavailable(error: unknown): boolean {
  const code = error && typeof error === "object" && "code" in error ? String((error as { code: unknown }).code) : "";
  return code === "DEPENDENCY_NOT_CONFIGURED" || code === "COMMUNICATIONS_UNAVAILABLE" || code === "COMMUNICATIONS_NOT_ACTIVATED";
}
