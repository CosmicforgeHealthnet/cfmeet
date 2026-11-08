/**
 * Voicemail (owner view). Mirrors packages/contracts/calling/enterprise/v1.schema.json
 * (VoicemailMailbox, VoicemailMessage, VoicemailMediaAccess, SetVoicemailReadStateRequest).
 * The web adapter always sends `principalId` = the signed-in principal (CAL-006), so Calling
 * answers with the owner view (`VoicemailRequester::Principal`), never the administrator view.
 */

import type { Uuid } from "./common";

export type MailboxOwnerType = "PRINCIPAL" | "EXTENSION" | "QUEUE" | "RING_GROUP";
export type VoicemailMessageState = "RECORDING" | "PROCESSING" | "READY" | "FAILED" | "DELETING" | "DELETED";
export type VoicemailReadState = "UNREAD" | "READ";
export type ResourceStatus = "ACTIVE" | "DISABLED";

export interface VoicemailMailbox {
  mailboxId: Uuid;
  owner: { type: MailboxOwnerType; id: Uuid };
  displayLabel: string;
  maxMessageSeconds: number;
  retentionDays: number;
  status: ResourceStatus;
  version: number;
  createdAt: string;
  updatedAt: string;
  disabledAt: string | null;
}

/** The adapter's "my mailboxes" answer (bounded discovery; `truncated` when the bound was hit). */
export interface VoicemailMailboxList {
  mailboxes: VoicemailMailbox[];
  nextCursor: string | null;
  truncated?: boolean;
}

export interface VoicemailMessage {
  messageId: Uuid;
  mailboxId: Uuid;
  callId: Uuid;
  callerPrincipalId: Uuid;
  captureSource: string;
  state: VoicemailMessageState;
  readState: VoicemailReadState;
  contentType: "audio/wav" | "audio/ogg" | null;
  sizeBytes: number | null;
  durationMs: number | null;
  sha256: string | null;
  receivedAt: string;
  readyAt: string | null;
  readAt: string | null;
  failureCode: string | null;
  retentionUntil: string | null;
  deletedAt: string | null;
  version: number;
}

export interface VoicemailMessageList {
  messages: VoicemailMessage[];
  nextCursor: string | null;
}

export interface VoicemailMessageFilters {
  mailboxId: Uuid;
  readState?: VoicemailReadState;
  limit?: number;
  cursor?: string;
}

/** Short-lived (at most 900 s) signed URL of a READY message. */
export interface MediaAccessResult {
  messageId: Uuid;
  url: string;
  expiresAt: string;
  contentType: "audio/wav" | "audio/ogg";
  sizeBytes: number;
  sha256: string;
}

export interface ReadStateRequest {
  expectedVersion: number;
  readState: VoicemailReadState;
}
