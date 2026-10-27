/**
 * A principal's call history (Part F §62).
 * Mirrors the CallHistory* components of packages/contracts/calling/http/v1.openapi.json.
 */

import type { CallKind, CallState, Uuid } from "./common";
import type { CallCore } from "./call";

export type HistoryDirection = "INBOUND" | "OUTBOUND";

export type ParticipantOutcome =
  | "ANSWERED"
  | "NOT_ANSWERED"
  | "MISSED"
  | "DECLINED"
  | "ANSWERED_ELSEWHERE"
  | "RINGING"
  | "NOT_REACHED"
  | "IN_PROGRESS";

export type HistoryDestinationType =
  | "EXTENSION"
  | "RING_GROUP"
  | "QUEUE"
  | "IVR"
  | "VOICEMAIL";

export interface CallParticipation {
  participantId: Uuid;
  direction: HistoryDirection;
  outcome: ParticipantOutcome;
  missed: boolean;
  addedAt: string;
  answeredAt: string | null;
  leftAt: string | null;
}

export interface CallCounterpart {
  principalId: Uuid;
  direction: HistoryDirection;
  displayName: string | null;
}

export interface CallHistoryEnterprise {
  entryExtension: string;
  entryLabel: string;
  phase: string;
  outcome: string | null;
  ringGroupLabel: string | null;
  queueLabel: string | null;
}

export interface CallHistoryVoicemail {
  messageId: Uuid;
  state: "RECORDING" | "PROCESSING" | "READY" | "FAILED" | "DELETING";
  readState: "UNREAD" | "READ";
}

export interface CallHistoryEntry extends CallCore {
  kind: CallKind;
  state: CallState;
  durationMs: number | null;
  participation: CallParticipation;
  counterparts: CallCounterpart[];
  enterprise: CallHistoryEnterprise | null;
  voicemail: CallHistoryVoicemail | null;
  recordingAvailable: boolean;
}

export interface CallHistoryList {
  principalId: Uuid;
  data: CallHistoryEntry[];
  nextCursor: string | null;
}

export interface CallHistoryFilters {
  limit?: number;
  cursor?: string;
  state?: CallState;
  createdAfter?: string;
  createdBefore?: string;
  initiatorPrincipalId?: Uuid;
  direction?: HistoryDirection;
  outcome?: ParticipantOutcome;
  missed?: boolean;
  enterpriseDestinationType?: HistoryDestinationType;
  enterpriseDestinationId?: Uuid;
}

export interface CallListFilters {
  limit?: number;
  cursor?: string;
  state?: CallState;
  createdAfter?: string;
  createdBefore?: string;
  initiatorPrincipalId?: Uuid;
  principalId?: Uuid;
  direction?: HistoryDirection;
  outcome?: ParticipantOutcome;
  missed?: boolean;
  enterpriseDestinationType?: HistoryDestinationType;
  enterpriseDestinationId?: Uuid;
}
