/**
 * Call Recording & Consent types for the Calling API.
 * Manages recording permissions, consent, and recording lifecycle.
 */

import type { Uuid } from "./common";

/** Participant's consent decision for recording */
export type RecordingConsentDecision = "ACCEPTED" | "DECLINED" | "WITHDRAWN";

/** State of a recording */
export type RecordingState = "PENDING" | "RECORDING" | "PAUSED" | "STOPPED" | "FAILED";

/** A participant's recording authorization decision */
export interface RecordingAuthorization {
  participantId: Uuid;
  principalId: Uuid;
  displayName?: string;
  decision: RecordingConsentDecision;
  decidedAt: string;
}

/** List of recording authorizations for a call */
export interface RecordingAuthorizationList {
  callId: Uuid;
  data: RecordingAuthorization[];
}

/** A call recording */
export interface CallRecording {
  recordingId: Uuid;
  callId: Uuid;
  state: RecordingState;
  startedAt?: string;
  stoppedAt?: string;
  durationMs?: number;
  sizeBytes?: number;
  mediaUrl?: string;
  mediaUrlExpiresAt?: string;
  createdAt: string;
}

/** List of recordings for a call */
export interface CallRecordingList {
  callId: Uuid;
  data: CallRecording[];
}

/** Request body for recording consent */
export interface RecordConsentRequest {
  decision: RecordingConsentDecision;
  participantId?: Uuid;
}

/** Result of recording consent */
export interface RecordConsentResult {
  callId: Uuid;
  participantId: Uuid;
  decision: RecordingConsentDecision;
  recorded: boolean;
}

/** Request body for starting a recording */
export interface StartRecordingRequest {
  layout?: string;
}

/** Result of starting a recording */
export interface StartRecordingResult {
  callId: Uuid;
  recording: CallRecording;
  started: boolean;
}

/** Result of stopping a recording */
export interface StopRecordingResult {
  callId: Uuid;
  recordingId: Uuid;
  recording: CallRecording;
  stopped: boolean;
}
