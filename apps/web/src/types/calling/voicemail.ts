/**
 * Voicemail types for the Calling API.
 * Manages voicemail mailboxes and messages.
 */

import type { Uuid } from "./common";

/** Type of voicemail greeting */
export type VoicemailGreetingType = "DEFAULT" | "BUSY" | "UNAVAILABLE" | "CUSTOM";

/** State of a voicemail message */
export type VoicemailMessageState = "RECORDING" | "PROCESSING" | "READY" | "FAILED" | "DELETING";

/** Read state of a voicemail message */
export type VoicemailReadState = "UNREAD" | "READ";

/** A voicemail mailbox */
export interface VoicemailMailbox {
  mailboxId: Uuid;
  principalId: Uuid;
  name: string;
  extension?: string;
  greetingType: VoicemailGreetingType;
  customGreetingUrl?: string;
  maxMessageDurationSeconds: number;
  retentionDays: number;
  emailNotification: boolean;
  notificationEmail?: string;
  transcriptionEnabled: boolean;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

/** List of voicemail mailboxes */
export interface VoicemailMailboxList {
  data: VoicemailMailbox[];
}

/** Request body for creating a mailbox */
export interface CreateMailboxRequest {
  name: string;
  extension?: string;
  greetingType?: VoicemailGreetingType;
  maxMessageDurationSeconds?: number;
  retentionDays?: number;
  emailNotification?: boolean;
  notificationEmail?: string;
  transcriptionEnabled?: boolean;
}

/** Request body for updating a mailbox */
export interface UpdateMailboxRequest {
  name?: string;
  greetingType?: VoicemailGreetingType;
  customGreetingUrl?: string;
  maxMessageDurationSeconds?: number;
  retentionDays?: number;
  emailNotification?: boolean;
  notificationEmail?: string;
  transcriptionEnabled?: boolean;
  enabled?: boolean;
}

/** A voicemail message */
export interface VoicemailMessage {
  messageId: Uuid;
  mailboxId: Uuid;
  callId?: Uuid;
  callerPrincipalId?: Uuid;
  callerDisplayName?: string;
  callerNumber?: string;
  state: VoicemailMessageState;
  readState: VoicemailReadState;
  durationSeconds: number;
  transcription?: string;
  transcriptionConfidence?: number;
  createdAt: string;
  readAt?: string;
}

/** Paginated list of voicemail messages */
export interface VoicemailMessageList {
  mailboxId: Uuid;
  data: VoicemailMessage[];
  nextCursor?: string | null;
  unreadCount: number;
}

/** Filters for listing voicemail messages */
export interface VoicemailMessageFilters {
  mailboxId: Uuid;
  readState?: VoicemailReadState;
  limit?: number;
  cursor?: string;
}

/** Result of requesting media access */
export interface MediaAccessResult {
  messageId: Uuid;
  url: string;
  expiresAt: string;
}

/** Request body for setting read state */
export interface ReadStateRequest {
  readState: VoicemailReadState;
}
