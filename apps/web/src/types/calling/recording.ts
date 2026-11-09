/**
 * Call recording consent and controls (participant view). Mirrors
 * packages/contracts/calling/enterprise/v1.schema.json (RecordingAuthorization,
 * RecordingConsentRequest, RecordingConsent, CallRecording, CallRecordingEnvelope). The adapter
 * binds every request to the signed-in participant (CAL-005).
 */

import type { Uuid } from "./common";

export type ConsentDecision = "GRANTED" | "DECLINED";

export type RecordingState =
  | "STARTING" | "RECORDING" | "STOPPING" | "PROCESSING" | "READY" | "FAILED" | "DELETING" | "DELETED";

export type RecordingMode = "NONE" | "DISABLED" | "MANUAL_ALLOWED" | "AUTO_WHEN_AUTHORIZED";

export type RecordingAuthorizationReason =
  | "AUTHORIZED" | "RECORDING_DISABLED" | "NOT_AN_ENTERPRISE_CALL" | "NO_RECORDING_POLICY" | "POLICY_DISABLED"
  | "AUTO_RECORDING_NOT_CONFIGURED" | "CALL_STATE_NOT_ELIGIBLE" | "ACTOR_NOT_PARTICIPANT" | "NOTICE_UNDELIVERABLE"
  | "CONSENT_MISSING" | "CONSENT_DECLINED" | "ARTIFACT_STORAGE_UNAVAILABLE" | "CAPTURE_UNAVAILABLE";

export type RecordingStopReason =
  | "EXPLICIT" | "CALL_ENDED" | "POLICY_REVOKED" | "CONSENT_WITHDRAWN" | "CAPTURE_FAILED" | "MAX_DURATION" | "KILL_SWITCH";

export interface RecordingAuthorization {
  authorizationId: Uuid;
  callId: Uuid;
  recordingPolicyId: Uuid | null;
  policyVersion: number | null;
  mode: RecordingMode;
  jurisdictionRef: string | null;
  actor: Record<string, unknown>;
  requiredParticipantIds: Uuid[];
  notice: { required: boolean; deliveries?: Array<{ participantId: Uuid; channel: "APPLICATION_SIGNAL" | "EDGE_TONE" }> };
  consent: { required: boolean; participants?: Array<{ participantId: Uuid; consentId: Uuid | null; decision: ConsentDecision | null }> };
  decision: "ALLOWED" | "DENIED";
  reason: RecordingAuthorizationReason;
  createdAt: string;
}

export interface RecordingAuthorizationList {
  authorizations: RecordingAuthorization[];
}

export interface CallRecording {
  recordingId: Uuid;
  callId: Uuid;
  authorizationId: Uuid;
  recordingPolicyId: Uuid;
  captureSource: "SIP" | "APPLICATION";
  state: RecordingState;
  startedAt: string;
  stoppedAt: string | null;
  stopReason: RecordingStopReason | null;
  contentType: "audio/wav" | "audio/ogg" | null;
  sizeBytes: number | null;
  durationMs: number | null;
  sha256: string | null;
  readyAt: string | null;
  failureCode: string | null;
  retentionUntil: string | null;
  deletedAt: string | null;
  version: number;
}

export interface CallRecordingList {
  recordings: CallRecording[];
  nextCursor: string | null;
}

export interface CallRecordingEnvelope {
  recording: CallRecording;
}

/** One participant's own decision through its own endpoint (there is no "everyone consented" flag). */
export interface RecordConsentRequest {
  endpointId: Uuid;
  decision: ConsentDecision;
}

export interface RecordingConsent {
  consentId: Uuid;
  callId: Uuid;
  participantId: Uuid;
  decision: ConsentDecision;
  recordedAt: string;
}
