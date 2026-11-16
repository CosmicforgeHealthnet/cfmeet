/**
 * Call representations, ring offers, transfers and call-control results.
 * Mirrors packages/contracts/calling/common/v1.schema.json and .../control/v1.schema.json.
 */

import type {
  CallActionName,
  CallActionUnavailableReason,
  CallControlOperation,
  CallEndpointCategory,
  CallFailureCategory,
  CallInvitationPurpose,
  CallInvitationStatus,
  CallInvitationTerminalReason,
  CallKind,
  CallMediaAccess,
  CallParticipantRole,
  CallSessionState,
  CallState,
  CallTerminalReason,
  CallTransferFailureReason,
  CallTransferState,
  PrincipalKind,
  Uuid,
} from "./common";

export interface CallCore {
  callId: Uuid;
  kind: CallKind;
  state: CallState;
  version: number;
  initiatorPrincipalId: Uuid;
  terminalReason: CallTerminalReason | null;
  failureCategory: CallFailureCategory | null;
  requestedAt: string;
  authorizedAt: string | null;
  ringingAt: string | null;
  answeredAt: string | null;
  connectedAt: string | null;
  endingAt: string | null;
  endedAt: string | null;
  createdAt: string;
  updatedAt: string;
}

export type CallSummary = CallCore;

export interface CallParticipant {
  participantId: Uuid;
  principalId: Uuid;
  principalKind: PrincipalKind;
  role: CallParticipantRole;
  displayName: string | null;
  createdAt: string;
  endedAt: string | null;
  connectedDurationMs: number;
}

export interface CallSession {
  sessionId: Uuid;
  participantId: Uuid;
  endpointCategory: CallEndpointCategory;
  state: CallSessionState;
  connectionCount: number;
  createdAt: string;
  firstConnectedAt: string | null;
  lastDisconnectedAt: string | null;
  endedAt: string | null;
  connectedDurationMs: number;
  endpointId: Uuid | null;
  mediaAccess: CallMediaAccess;
  mediaAdmittedAt: string | null;
  removedAt: string | null;
}

export interface CallInvitation {
  invitationId: Uuid;
  participantId: Uuid;
  endpointId: Uuid;
  purpose: CallInvitationPurpose;
  status: CallInvitationStatus;
  invitationGeneration: number;
  issuedAt: string;
  expiresAt: string;
  finishedAt: string | null;
  terminalReason: CallInvitationTerminalReason | null;
  sessionId: Uuid | null;
}

export interface CallTransfer {
  transferId: Uuid;
  transferringParticipantId: Uuid;
  targetPrincipalId: Uuid;
  targetParticipantId: Uuid | null;
  state: CallTransferState;
  preTransferState: "CONNECTED" | "HELD";
  failureReason: CallTransferFailureReason | null;
  failureCategory: CallFailureCategory | null;
  initiatedAt: string;
  completedAt: string | null;
  failedAt: string | null;
  cancelledAt: string | null;
}

export interface Call extends CallCore {
  /** Whether this caller may read the call's recordings (ask for them only when true). */
  canViewRecordings?: boolean;
  connectedDurationMs: number | null;
  durationsAsOf: string;
  participants: CallParticipant[];
  sessions: CallSession[];
  invitations: CallInvitation[];
  transfers: CallTransfer[];
  /** C6: who is presenting a screen now (null when nobody is). */
  screenShare?: ScreenShare | null;
}

/** C6: the Call's current presenter. */
export interface ScreenShare {
  participantId: Uuid;
  startedAt: string;
}

export interface ScreenShareResult {
  call: CallSummary;
  operation: "START_SCREEN_SHARE" | "STOP_SCREEN_SHARE";
  screenShare: ScreenShare | null;
}

/** C7: result of inviting people into an ongoing call. */
export interface AddParticipantsResult {
  call: CallSummary;
  operation: "ADD_PARTICIPANTS";
  invitingParticipantId: Uuid;
  participants: { participantId: Uuid; principalId: Uuid; role: "RECIPIENT"; liveInvitationCount: number }[];
}

export interface CallList {
  data: CallSummary[];
  nextCursor: string | null;
}

export interface AvailableAction {
  action: CallActionName;
  available: boolean;
  reason?: CallActionUnavailableReason;
}

export interface AvailableActions {
  callId: Uuid;
  callState: CallState;
  callVersion: number;
  actions: AvailableAction[];
}

export interface CreatedParticipant {
  participantId: Uuid;
  principalId: Uuid;
  role: CallParticipantRole;
}

export interface CreateCallResult {
  call: CallSummary;
  initiatorParticipantId: Uuid;
  initiatorSessionId: Uuid | null;
  participants: CreatedParticipant[];
  liveInvitationCount: number;
  enterprise?: unknown;
}

export interface AcceptResult {
  call: CallSummary;
  invitationId: Uuid;
  participantId: Uuid;
  sessionId: Uuid;
  outcome: "ACCEPTED";
  callAnswered: boolean;
}

/** POST /v1/calling/invitations/{id}/acknowledge: the device received the offer (C1). */
export interface AcknowledgeResult {
  invitationId: Uuid;
  callId: Uuid;
  status: "PENDING" | "RINGING";
  ringingAt: string | null;
  expiresAt: string;
  callState: CallState;
  callVersion: number;
}

export interface DeclineResult {
  call: CallSummary;
  invitationId: Uuid;
  participantId: Uuid;
  outcome: "DECLINED";
}

export interface ControlResult {
  call: CallSummary;
  operation: CallControlOperation;
}

export interface SessionRemovalResult {
  call: CallSummary;
  operation: "REMOVE_SESSION";
  sessionId: Uuid;
  removed: true;
}

export interface TransferResult {
  call: CallSummary;
  transfer: CallTransfer;
}
