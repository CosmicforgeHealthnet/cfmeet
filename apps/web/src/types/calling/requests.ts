/**
 * Request bodies of the application-calling API.
 * Mirrors packages/contracts/calling/control/v1.schema.json.
 */

import type { CallKind, Uuid } from "./common";

export interface CreateCallRequest {
  kind: CallKind;
  initiatorPrincipalId: Uuid;
  initiatorEndpointId?: Uuid;
  targetPrincipalIds?: Uuid[];
  initiatorDisplayName?: string;
  privacyRelaySessionId?: Uuid;
  targetContactReference?: Uuid;
  callbackRequested?: boolean;
  initiatorContactReference?: Uuid;
  targetExtension?: string;
  presentedNumberResourceId?: Uuid;
}

/** Body of cancel / end / hold / resume and session removal. */
export interface CommandRequest {
  expectedVersion: number;
}

/** Body of the backend accept / decline (answering for one endpoint). */
export interface InvitationCommandRequest {
  invitationId: Uuid;
  endpointId: Uuid;
  expectedVersion: number;
}

export interface TransferRequest {
  transferringParticipantId: Uuid;
  expectedVersion: number;
  targetPrincipalId?: Uuid;
  targetExtension?: string;
  targetDestination?: {
    type: "PRINCIPAL" | "ENTERPRISE_MEMBER" | "EXTENSION" | "RING_GROUP" | "QUEUE" | "IVR" | "VOICEMAIL" | "SCHEDULE" | "PUBLIC_CONTACT";
    id: Uuid;
  };
}

export interface IvrInputRequest {
  endpointId: Uuid;
  digit: "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "*" | "#";
}
