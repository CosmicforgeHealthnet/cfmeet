/**
 * The Calling service error envelope and its stable public error codes.
 * Mirrors the ErrorEnvelope / ErrorCode components of the Calling OpenAPI contract.
 */

export type CallingErrorCode =
  | "AUTHENTICATION_FAILED"
  | "INSUFFICIENT_SCOPE"
  | "NOT_FOUND"
  | "METHOD_NOT_ALLOWED"
  | "VALIDATION_ERROR"
  | "RATE_LIMITED"
  | "SERVICE_UNAVAILABLE"
  | "INTERNAL_ERROR"
  | "STALE_CALL_VERSION"
  | "CALL_STATE_CONFLICT"
  | "CALL_EVIDENCE_REQUIRED"
  | "CALL_NOT_AUTHORIZED"
  | "CONCURRENT_CALL_LIMIT_EXCEEDED"
  | "CALL_RESOURCE_STATE_CONFLICT"
  | "CONFLICT"
  | "IDEMPOTENCY_CONFLICT"
  | "INVITATION_EXPIRED"
  | "INVITATION_NOT_ACTIONABLE"
  | "ANSWERED_ELSEWHERE"
  | "CAPABILITY_UNAVAILABLE"
  | "MEDIA_UNAVAILABLE"
  | "PARTICIPANT_LIMIT_EXCEEDED"
  | "TARGET_UNAVAILABLE"
  | "PRIVACY_AUTHORIZATION_UNAVAILABLE"
  | "PUBLIC_ROUTE_DENIED"
  | "PUBLIC_ROUTE_UNAVAILABLE"
  | "TELECOM_SPEND_LIMIT_EXCEEDED"
  | "VERSION_CONFLICT"
  | "ENTERPRISE_POLICY_DENIED"
  | "ARTIFACT_STORAGE_UNAVAILABLE"
  | "RECORDING_NOT_AUTHORIZED"
  | "PRINCIPAL_NOT_ELIGIBLE";

export interface CallingErrorBody {
  code: CallingErrorCode | string;
  message: string;
  requestId: string | null;
}

export interface CallingErrorEnvelope {
  error: CallingErrorBody;
}
