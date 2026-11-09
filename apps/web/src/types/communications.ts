/**
 * Communications Hub public contract (`cfmeet.communications.http-api` 1.2.0,
 * packages/contracts/communications/http/v1.openapi.json) — the subset the web client uses.
 * Paths are relative to `/api/v2/communications`; the Account bearer session is the only credential.
 */
export type RelationshipState = "NONE" | "REQUEST_PENDING" | "CONNECTED" | "BLOCKED" | "REVOKED";
export type ActivationState = "ACCOUNT_CREATED" | "ACTIVE" | "SUSPENDED" | "DEACTIVATED";
export type ContactNotifications = "ALL" | "MUTED" | "MENTIONS";

export interface CommunicationsProfile {
  schemaVersion: 1;
  profileId: string;
  cfMeetId: string;
  displayName: string;
  avatarReference: string | null;
  activationState: ActivationState;
  version: number;
  createdAt: string;
  updatedAt: string;
}

export interface BackendCapability {
  available: boolean;
  reason: string;
  securityReady?: boolean;
}

export interface CommunicationsCapabilities {
  schemaVersion: 1;
  activationState: ActivationState;
  personalCalling: BackendCapability;
  messaging: {
    available: boolean;
    reason: string;
    securityReady: boolean;
    managed: { available: boolean; reason: string; realtime: "SESSION_DEVICE" | "POLLING" | "NONE" };
    privateE2ee: { available: boolean; reason: string };
  };
  contacts: { available: boolean };
  meetings: { independent: true; authorization: "EXISTING_MEETING_RULES" };
}

export interface PublicId {
  schemaVersion: 1;
  cfMeetId: string;
  version: number;
}

export interface Invite {
  schemaVersion: 1;
  inviteId: string;
  purpose: "CONNECT";
  expiresAt: string;
  maxUses: number;
  uses: number;
  version: number;
  secretReturned: boolean;
  /** `{public app}/connect/{token}`: disclosed once, never listed again. */
  payload?: string;
}

export interface InviteMetadata {
  inviteId: string;
  purpose: "CONNECT";
  expiresAt: string;
  revoked: boolean;
  maxUses: number;
  uses: number;
  version: number;
}

export interface PermittedActions {
  requestConnection: boolean;
  directCallEligible: boolean;
  directChatEligible: boolean;
}

export interface PublicProfile {
  schemaVersion: 1;
  cfMeetId: string;
  displayName: string;
  avatarReference: string | null;
  relationship: RelationshipState | null;
  permittedActions?: PermittedActions;
}

export interface ContactRequest {
  schemaVersion: 1;
  requestId: string;
  cfMeetId: string;
  state: "PENDING" | "ACCEPTED" | "DECLINED" | "CANCELLED" | "EXPIRED";
  version: number;
  expiresAt?: string;
  relationship?: RelationshipState;
}

export interface Contact {
  schemaVersion: 1;
  contactId: string;
  cfMeetId: string;
  displayName: string;
  avatarReference: string | null;
  relationship: RelationshipState;
  relationshipVersion: number;
  alias: string | null;
  favorite: boolean;
  notifications: ContactNotifications;
  version: number;
}

export interface Page<T> {
  schemaVersion: 1;
  data: T[];
  nextCursor: string | null;
}

export interface BlockMetadata {
  cfMeetId: string;
  blocked: true;
}

export interface Block {
  schemaVersion: 1;
  cfMeetId: string;
  blocked: boolean;
  relationship: RelationshipState;
}

export interface Privacy {
  publicIdDiscoverable: boolean;
  invitationPermissions: "ENABLED" | "DISABLED";
  contactRequestPermissions: "EVERYONE" | "NOBODY";
  incomingCallPolicy: "CONNECTIONS" | "NOBODY";
  directChatPolicy: "CONNECTIONS" | "NOBODY";
  publicProfileVisibility: "PUBLIC" | "CONNECTIONS" | "HIDDEN";
  presenceVisibility: "CONNECTIONS" | "NOBODY";
}

export interface PrivacyView {
  schemaVersion: 1;
  version: number;
  privacy: Privacy;
}

export interface TimelineEntry {
  entryId: string;
  kind: "CALL" | "MEETING_INVITATION";
  source?: string;
  sourceId?: string;
  conversationId?: string;
  occurredAt: string;
  anchorSequence?: number | null;
  sourceVersion?: number;
  refreshedAt?: string;
  call?: Record<string, unknown> | null;
  meeting?: Record<string, unknown> | null;
}

export interface Timeline {
  schemaVersion: 1;
  projectionVersion: number;
  conversationId?: string;
  clearedThroughSequence?: number;
  stale: boolean;
  data: TimelineEntry[];
  nextCursor: string | null;
}

export interface ActivityPage {
  schemaVersion: 1;
  data: TimelineEntry[];
  nextCursor: string | null;
}

export interface Participant {
  self: boolean;
  known?: boolean;
  cfMeetId?: string;
  displayName?: string;
  relationship?: RelationshipState;
  contactId?: string | null;
  alias?: string | null;
}

export interface ConversationContext {
  schemaVersion: 1;
  conversationId: string;
  contextClass: string;
  securityProfile?: string;
  lifecycleState?: string;
  lastSequence?: number;
  memberCount?: number;
  role?: string | null;
  counterpart?: Participant | null;
  clearedThroughSequence?: number;
  linkedMeetings?: number;
  actions: {
    voiceCall?: boolean;
    videoCall?: boolean;
    instantMeeting?: boolean;
    shareMeeting?: boolean;
    clearForMe?: boolean;
    mute?: boolean;
  };
}

export interface InboxEntry {
  conversationId: string;
  notifications: ContactNotifications;
  mutedUntil: string | null;
  clearedThroughSequence: number;
  latestActivity: { entryId: string; kind: string; occurredAt: string; missed?: boolean } | null;
}

export interface MeetingLink {
  schemaVersion?: 1;
  linkId: string;
  conversationId: string;
  meetingSpaceId?: string;
  meetingCode: string;
  origin: "INSTANT" | "SHARED";
  linkedAt?: string;
  linkedByViewer?: boolean;
  conferenceId?: string | null;
  join?: Record<string, unknown>;
}

export interface CallHistoryEntry {
  callId: string;
  kind: string;
  state: string;
  terminal?: boolean;
  direction?: "INCOMING" | "OUTGOING";
  outcome?: string;
  missed: boolean;
  createdAt?: string;
  answeredAt?: string | null;
  endedAt?: string | null;
  durationMs?: number | null;
  counterparts: Participant[];
  conversationId?: string | null;
  origin?: string;
  actions?: Record<string, unknown>;
}

/** `TimelineEntry.call` / `.meeting` as the Hub renders them (calls.rs `timeline_view`). */
export interface TimelineCallCard {
  kind?: string;
  state?: string;
  terminal?: boolean;
  outcome?: string;
  missed?: boolean;
  direction?: string;
  durationMs?: number | null;
  initiatedByViewer?: boolean;
}
export interface TimelineMeetingCard {
  meetingCode?: string;
  origin?: string;
  sharedByViewer?: boolean;
}

export interface Change {
  position: number;
  domain: string;
  resourceId?: string | null;
  resourceVersion?: number | null;
  changedAt: string;
}

export interface Changes {
  schemaVersion: 1;
  resyncRequired: boolean;
  reason?: string;
  latestPosition: number;
  data: Change[];
  nextPosition: number;
  hasMore?: boolean;
}

export interface Bootstrap {
  schemaVersion: 1;
  me: Record<string, unknown>;
  capabilities: CommunicationsCapabilities;
  counts: Record<string, number>;
  sync: { latestPosition: number; [key: string]: unknown };
}

export interface ReportReceipt {
  schemaVersion: 1;
  [key: string]: unknown;
}
