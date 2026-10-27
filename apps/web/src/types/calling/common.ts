/**
 * Canonical, provider-independent call-domain vocabulary of the Calling API.
 * Mirrors packages/contracts/calling/common/v1.schema.json.
 */

export type CallState =
  | "REQUESTED"
  | "AUTHORIZED"
  | "RESOLVING"
  | "ROUTING"
  | "DIALING"
  | "RINGING"
  | "ANSWERED"
  | "CONNECTED"
  | "HELD"
  | "TRANSFERRING"
  | "ENDING"
  | "ENDED"
  | "DECLINED"
  | "BUSY"
  | "NO_ANSWER"
  | "FAILED"
  | "CANCELLED";

export type CallKind = "VOICE" | "VIDEO";

export type CallFailureCategory =
  | "POLICY"
  | "TARGET_UNAVAILABLE"
  | "ROUTING"
  | "TIMEOUT"
  | "MEDIA"
  | "NETWORK"
  | "EXECUTION"
  | "INTERNAL"
  | "UNKNOWN";

export type CallTerminalReason =
  | "CLEANUP_CONFIRMED"
  | "PARTICIPATION_ENDED"
  | "RECONCILIATION_TIMEOUT"
  | "DECLINED"
  | "BUSY"
  | "NO_ANSWER"
  | "FAILED"
  | "CANCELLED";

export type CallParticipantRole = "INITIATOR" | "RECIPIENT";

export type CallSessionState = "CREATED" | "CONNECTED" | "DISCONNECTED" | "ENDED";

export type CallEndpointCategory = "MOBILE" | "WEB" | "DESKTOP" | "SERVER" | "NETWORK";

export type CallEndpointKind = "WEB" | "MOBILE" | "DESKTOP" | "ENTERPRISE_APP";

export type CallEndpointStatus = "ACTIVE" | "REVOKED";

export type CallEndpointPresence = "ONLINE" | "OFFLINE";

export type CallMediaAccess = "NONE" | "AUTHORIZED" | "REVOKED";

export type CallInvitationPurpose = "CALL" | "TRANSFER";

export type CallInvitationStatus =
  | "PENDING"
  | "RINGING"
  | "ACCEPTED"
  | "DECLINED"
  | "ANSWERED_ELSEWHERE"
  | "CANCELLED"
  | "EXPIRED";

export type CallInvitationTerminalReason =
  | "ACCEPTED"
  | "DECLINED"
  | "ANSWERED_ELSEWHERE"
  | "EXPIRED"
  | "CALL_CANCELLED"
  | "CALL_TERMINATED"
  | "ENDPOINT_REVOKED"
  | "PARTICIPANT_REMOVED"
  | "TRANSFER_ENDED";

export type CallTransferState =
  | "REQUESTED"
  | "RINGING_TARGET"
  | "ACCEPTED"
  | "COMPLETED"
  | "FAILED"
  | "CANCELLED";

export type CallTransferFailureReason =
  | "TARGET_DECLINED"
  | "TARGET_NO_ANSWER"
  | "TARGET_UNAVAILABLE"
  | "MEDIA_PATH_LOST"
  | "CALL_ENDED"
  | "CANCELLED";

export type CallControlOperation = "CANCEL" | "END" | "HOLD" | "RESUME";

export type CallActionName =
  | "ACCEPT"
  | "DECLINE"
  | "CANCEL"
  | "END"
  | "HOLD"
  | "RESUME"
  | "TRANSFER"
  | "ISSUE_MEDIA_CREDENTIAL";

export type CallActionUnavailableReason =
  | "INSUFFICIENT_SCOPE"
  | "STATE"
  | "CAPABILITY_UNAVAILABLE";

export type PrincipalKind =
  | "NATIVE_CF_USER"
  | "EXTERNAL_APPLICATION_PRINCIPAL"
  | "ENTERPRISE_MEMBER"
  | "SERVICE_ACCOUNT"
  | "TELEPHONE_ENDPOINT"
  | "TEMPORARY_EXTERNAL_PARTICIPANT"
  | "BOT_OR_SERVICE";

export type Uuid = string;

/** Terminal states never transition again. */
export const TERMINAL_CALL_STATES: readonly CallState[] = [
  "ENDED",
  "DECLINED",
  "BUSY",
  "NO_ANSWER",
  "FAILED",
  "CANCELLED",
];

export function isTerminalCallState(state: CallState): boolean {
  return TERMINAL_CALL_STATES.includes(state);
}

/** A Call is live (answerable / in progress) in these states. */
export const LIVE_CALL_STATES: readonly CallState[] = [
  "REQUESTED",
  "AUTHORIZED",
  "RESOLVING",
  "ROUTING",
  "DIALING",
  "RINGING",
  "ANSWERED",
  "CONNECTED",
  "HELD",
  "TRANSFERRING",
  "ENDING",
];

export function isLiveCallState(state: CallState): boolean {
  return LIVE_CALL_STATES.includes(state);
}
