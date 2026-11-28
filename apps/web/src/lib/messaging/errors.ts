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

/** Plain-language text for the codes people actually run into (never a raw CODE_NAME). */
const FRIENDLY: Record<string, string> = {
  FEATURE_DISABLED: "Messaging is not enabled for this deployment.",
  CHAT_UNAVAILABLE: "Messaging is temporarily unavailable. Please try again.",
  SERVICE_UNAVAILABLE: "Messaging is temporarily unavailable. Please try again.",
  STORAGE_UNAVAILABLE: "Messaging is temporarily unavailable. Please try again.",
  INTERNAL_ERROR: "Something went wrong. Please try again.",
  RATE_LIMITED: "You're doing that too often. Wait a moment and try again.",
  QUOTA_EXCEEDED: "You've reached a limit for this. Try again later.",
  PAYLOAD_TOO_LARGE: "That's too large to send.",
  CONTENT_REJECTED: "That message can't be sent.",
  SEND_RESTRICTED: "Only admins can send messages in this chat.",
  CONVERSATION_NOT_ACTIVE: "This chat is closed, so nothing new can be sent.",
  NOT_A_MEMBER: "You're no longer in this chat.",
  SENDER_NOT_A_MEMBER: "You're no longer in this chat.",
  MEMBERSHIP_ENDED: "You're no longer in this chat.",
  PRINCIPAL_NOT_AUTHORIZED: "You don't have permission to do that.",
  INSUFFICIENT_SCOPE: "You don't have permission to do that.",
  ADMINISTRATION_ONLY: "Only group admins can do that.",
  MODERATOR_REQUIRED: "Only moderators can do that.",
  MESSAGE_NOT_MODIFIABLE: "This message can't be changed any more.",
  NOT_FOUND: "That isn't available any more.",
  ATTACHMENT_NOT_READY: "The file is still being checked. Try again in a moment.",
  LEGAL_HOLD_ACTIVE: "This content is on hold and can't be deleted.",
  DEVICE_REVOKED: "This browser was signed out of chat. Sign in again to continue.",
  AUTHORIZATION_REVOKED: "Your session has expired. Sign in again to continue.",
  CONTEXT_NOT_ALLOWED: "Chat isn't allowed here.",
  SECURITY_PROFILE_NOT_ALLOWED: "This kind of chat isn't allowed in your workspace.",
  DELIVERY_REFUSED: "This person can't receive your messages.",
  VERSION_CONFLICT: "Someone else changed this at the same time. Try again.",
  CONFLICT: "Someone else changed this at the same time. Try again.",
};
/** A stored error code (e.g. on a queued message) as text a person can read. */
export function friendlyErrorCode(code: string | null | undefined): string {
  if (!code) return "Something went wrong";
  return (FRIENDLY[code] ?? (/^[A-Z][A-Z0-9_]+$/.test(code) ? "Something went wrong" : code)).replace(/\.$/, "");
}
const looksLikeCode = (text: string) => /^[A-Z][A-Z0-9_]+$/.test(text.trim());

export function messagingErrorMessage(error: unknown): string {
  if (error instanceof MessagingApiError) {
    if (error.status === 401) return "Your session has expired. Sign in again to continue.";
    const friendly = FRIENDLY[error.code];
    if (friendly) return friendly;
    if (error.status === 0) return "You're offline or the connection dropped. Try again.";
    if (!looksLikeCode(error.message)) return error.message;
    if (error.status === 403) return "You don't have permission to do that.";
    if (error.status >= 500) return "Something went wrong. Please try again.";
    return "The messaging request failed. Please try again.";
  }
  return error instanceof Error && !looksLikeCode(error.message) ? error.message : "The messaging request failed. Please try again.";
}
