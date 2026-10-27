export class MessagingApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    public readonly reason: string | null,
    message = code,
  ) {
    super(message);
    this.name = "MessagingApiError";
  }
}

export function messagingErrorMessage(error: unknown): string {
  if (error instanceof MessagingApiError) {
    if (error.code === "FEATURE_DISABLED") return "Messaging is not enabled for this deployment.";
    if (error.status === 401) return "Your session has expired. Sign in again to continue.";
    if (error.code === "CHAT_UNAVAILABLE") return "Messaging is temporarily unavailable. Please try again.";
  }
  return error instanceof Error ? error.message : "The messaging request failed. Please try again.";
}
