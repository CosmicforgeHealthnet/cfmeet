/**
 * Recovered ring offers of one endpoint (signaling credential).
 * Mirrors packages/contracts/calling/control/v1.schema.json.
 */

import type {
  CallInvitationPurpose,
  CallInvitationStatus,
  CallInvitationTerminalReason,
  CallKind,
  CallState,
  Uuid,
} from "./common";

export interface RecoveredInvitation {
  invitationId: Uuid;
  callId: Uuid;
  callKind: CallKind;
  callState: CallState;
  callVersion: number;
  purpose: CallInvitationPurpose;
  status: CallInvitationStatus;
  actionable: boolean;
  issuedAt: string;
  expiresAt: string;
  terminalReason: CallInvitationTerminalReason | null;
  sessionId: Uuid | null;
}

export interface RecoveredInvitationList {
  endpointId: Uuid;
  data: RecoveredInvitation[];
}
