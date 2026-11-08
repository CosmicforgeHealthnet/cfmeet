// GENERATED FILE — do not edit by hand.
// Source: packages/contracts/openapi/meeting-service.json
// Regenerate: npm run contracts:generate (apps/web). CI fails when this file is stale.
/* eslint-disable */

export const CONTRACT_TITLE = "CosmicForge RTC API";
export const CONTRACT_VERSION = "1.4.0";

/** Access-policy configuration (no secrets). */
export interface AccessConfig {
  /** CUSTOM: registered server-side access adapter. */
  adapterRef?: string | null;
  /** DOMAIN_RESTRICTED / EXTERNAL_ONLY: verified email domains treated as internal. */
  allowedDomains?: Array<string>;
  /**
   * HEALTHCARE_APPOINTMENT: optional appointment reference forwarded to the
   * appointment authority (which alone decides).
   */
  appointmentRef?: string | null;
  /**
   * Fail closed when an authenticated session's revocation cannot be verified
   * (always on for HEALTHCARE and HIGH_SECURITY security profiles).
   */
  requireVerifiedRevocation?: boolean;
  /** CUSTOM waiting-room evaluator. */
  waitingRoomAdapterRef?: string | null;
}


export type AccessPolicy = "PUBLIC" | "PUBLIC_PASSCODE" | "AUTHENTICATED" | "INVITE_ONLY" | "ORGANIZATION_ONLY" | "DOMAIN_RESTRICTED" | "HEALTHCARE_APPOINTMENT" | "CUSTOM";


export interface AccessProfile {
  /** Lifetime of guest / external participant sessions. */
  guestSessionTtlSecs: number;
  /** PUBLIC / PUBLIC_PASSCODE entry permitted. */
  publicAccessAllowed: boolean;
  /** External participants must present an external authorization context. */
  requireExternalContext: boolean;
  /** Waiting room applies even when the space says OFF (hosts stay exempt). */
  waitingRoomForced: boolean;
}

/** `GET/PATCH /api/v2/me/profile` representation. */
export interface AccountProfile {
  /** `local` or `google`. */
  authType: string;
  /** `https://` avatar reference (`null` when not set). */
  avatarUrl?: string | null;
  createdAt: string;
  /** Chosen display name (`null` when not set). */
  displayName?: string | null;
  /** What other people see: `displayName`, otherwise `username`. */
  effectiveDisplayName: string;
  email: string;
  emailVerified: boolean;
  emailVerifiedAt?: string | null;
  /** BCP 47 language tag (`null`: clients use the device language). */
  locale?: string | null;
  phoneVerification: PhoneVerificationStatus;
  profileUpdatedAt?: string | null;
  /** Canonical account status: `pending_verification`, `active`, `inactive` or `suspended`. */
  status: string;
  /** IANA time zone (`null`: clients use the device zone). */
  timezone?: string | null;
  userId: string;
  username: string;
}


export interface AccountSessionListResponse {
  sessions: Array<SessionRecord>;
}


export interface AccountSessionRevokeResponse {
  /** Number of sessions this request ended (0 when the session had already ended). */
  revokedCount: number;
}


export interface AddMemberRequest {
  /** Matched only against verified account emails. */
  email?: string | null;
  role: Role;
  /** Registered CF Meet user. Exactly one of `userId` / `email`. */
  userId?: string | null;
}

/** Paginated response for admin logs */
export interface AdminLogListResponse {
  data: Array<AdminLogResponse>;
  meta: AdminPaginationMeta;
  success: boolean;
}

/** Admin view of a session log entry */
export interface AdminLogResponse {
  event_time: string;
  event_type: string;
  id: string;
  meeting_id: string;
  meeting_title: string;
  metadata?: Record<string, unknown> | null;
  participant_id?: string | null;
  participant_name?: string | null;
}

/** Paginated response for admin meeting list */
export interface AdminMeetingListResponse {
  data: Array<AdminMeetingResponse>;
  meta: AdminPaginationMeta;
  success: boolean;
}

/** Admin view of a meeting */
export interface AdminMeetingResponse {
  created_at: string;
  end_time?: string | null;
  host_id: string;
  host_username: string;
  id: string;
  is_private: boolean;
  meeting_identifier: string;
  participant_count: number;
  start_time: string;
  status: string;
  title: string;
}

/** Pagination metadata for admin endpoints */
export interface AdminPaginationMeta {
  limit: number;
  page: number;
  total: number;
  total_pages: number;
}

/** Request to update a user (admin only) */
export interface AdminUpdateUserRequest {
  /** Role: user, admin, guest */
  role?: string | null;
  /** Status: pending_verification, active, inactive */
  status?: string | null;
}

/** API response wrapper for single admin user */
export interface AdminUserApiResponse {
  data: AdminUserResponse;
  success: boolean;
}

/** Paginated response for admin user list */
export interface AdminUserListResponse {
  data: Array<AdminUserResponse>;
  meta: AdminPaginationMeta;
  success: boolean;
}

/** Admin view of a user */
export interface AdminUserResponse {
  auth_type: string;
  created_at: string;
  email: string;
  id: string;
  last_login?: string | null;
  role: string;
  status: string;
  username: string;
}


export type AdmissionState = "WAITING" | "ADMITTED" | "DENIED" | "REMOVED" | "BANNED";


export interface AdmitRequest {
  /** Admit every participant currently WAITING. */
  all?: boolean;
  participantIds?: Array<string>;
}

/** One occurrence on the caller's agenda. */
export interface AgendaItem {
  /**
   * The conference id; null for a recurring occurrence beyond the materialization
   * horizon (it becomes a conference later, under the same `occurrenceKey`).
   */
  conferenceId?: string | null;
  /** FULL view only. */
  endedAt?: string | null;
  isException: boolean;
  kind: ConferenceKind;
  /** False for an occurrence computed from the recurrence rule (not yet materialized). */
  materialized: boolean;
  meetingSpace: AgendaSpace;
  meetingSpaceId: string;
  /** Stable occurrence identity within the meeting space (unchanged by materialization). */
  occurrenceKey: string;
  /** FULL view only. */
  originalStartAt?: string | null;
  relations: Array<AgendaRelation>;
  scheduledEndAt?: string | null;
  scheduledStartAt: string;
  /** FULL view only. */
  startedAt?: string | null;
  status: ConferenceStatus;
  view: ResourceView;
  viewerRole?: Role | null;
}

/** `GET /api/v2/me/conferences` query. */
export interface AgendaQuery {
  /** Opaque cursor from a previous page's `nextCursor`. */
  cursor?: string | null;
  /** Inclusive lower bound on the scheduled start (RFC 3339). Default: now. */
  from?: string | null;
  /** Page size, 1-200 (default 50). */
  limit?: number | null;
  /** Restrict to one organization's meetings (the caller must be a verified member). */
  organizationId?: string | null;
  /** Comma-separated relations of the caller (`AgendaRelation`), e.g. `OWNER,INVITEE`. */
  role?: string | null;
  /** Comma-separated conference statuses, e.g. `CREATED,LIVE`. */
  status?: string | null;
  /** Exclusive upper bound (RFC 3339). Default: `from` + 31 days; at most 366 days after `from`. */
  to?: string | null;
}

/** Why an occurrence is on the caller's agenda. */
export type AgendaRelation = "OWNER" | "MEMBER" | "INVITEE" | "REGISTRANT" | "PARTICIPANT" | "ORGANIZATION";


export interface AgendaResponse {
  /** Ordered by scheduled start, then meeting space, then occurrence key. */
  items: Array<AgendaItem>;
  /** Pass back as `cursor` for the next page; null on the last page. */
  nextCursor?: string | null;
}

/** Meeting-space fields an agenda entry may show. */
export interface AgendaSpace {
  accessPolicy: AccessPolicy;
  meetingCode: string;
  meetingType: MeetingType;
  /** FULL view only. */
  organizationId?: string | null;
  requiresPasscode: boolean;
  status: SpaceStatus;
  /** IANA timezone of the meeting: show times in this zone and in the viewer's zone. */
  timezone: string;
  /** Omitted for HEALTHCARE / HIGH_SECURITY spaces unless the caller manages the space. */
  title?: string | null;
}


export interface AnnouncementResponse {
  rooms: number;
}


export interface AnswerQuestionRequest {
  /** Optional written answer (omit when answered live). */
  answer?: string | null;
}

/**
 * Request to create a meeting via API key
 *
 * **Note**: `start_time` and `end_time` are in the user's local timezone.
 * The `timezone` field must be a valid IANA timezone string.
 */
export interface ApiCreateMeetingRequest {
  /** End time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS" */
  end_time?: string | null;
  is_private?: boolean | null;
  metadata?: unknown | null;
  /** Start time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS" */
  start_time: string;
  /** IANA timezone of the user (e.g., "Africa/Lagos", "America/New_York") */
  timezone: string;
  title: string;
}

/** Request to join a meeting via API key */
export interface ApiJoinMeetingRequest {
  display_name: string;
}

/** API response wrapper for single API key */
export interface ApiKeyApiResponse {
  data: ApiKeyResponse;
  success: boolean;
}

/** Response for API key info (masked key) */
export interface ApiKeyResponse {
  /** Masked API key: the first 8 characters (non-secret lookup prefix) followed by asterisks */
  api_key_masked: string;
  created_at: string;
  expires_at: string;
  id: string;
  status: string;
  updated_at: string;
  usage_limit: number;
  used_count: number;
}


export interface ArtifactAccess {
  artifactId: string;
  contentType?: string | null;
  expiresAt: string;
  expiresInSeconds: number;
  sizeBytes?: number | null;
  url: string;
}


export interface ArtifactListResponse {
  artifacts: Array<ArtifactView>;
}


export type ArtifactStatus = "REQUESTED" | "RECORDING" | "STOPPING" | "PROCESSING" | "READY" | "FAILED" | "DELETED";


export type ArtifactType = "RECORDING" | "TRANSCRIPT" | "CAPTION_TRACK" | "ATTENDANCE_REPORT" | "CHAT_EXPORT" | "POLL_REPORT" | "MEETING_SUMMARY" | "WHITEBOARD_SNAPSHOT";

/**
 * Public artefact projection: never exposes storage keys, provider references,
 * internal diagnostics or signed URLs.
 */
export interface ArtifactView {
  artifactType: ArtifactType;
  checksumSha256?: string | null;
  conferenceId: string;
  contentType?: string | null;
  createdAt: string;
  /** Deletion requested; no new access is issued. */
  deletionPending: boolean;
  durationMs?: number | null;
  endedAt?: string | null;
  /** Public-safe failure category (internal diagnostics are never returned). */
  failureCode?: string | null;
  finalizedAt?: string | null;
  id: string;
  layout?: string | null;
  /** Server-generated, type-specific metadata. */
  metadata: Record<string, unknown>;
  retentionAt?: string | null;
  sizeBytes?: number | null;
  sourceArtifactId?: string | null;
  startedAt?: string | null;
  status: ArtifactStatus;
  updatedAt: string;
}


export interface AssignBreakoutsRequest {
  /** MANUAL: explicit participant → breakout mapping. */
  assignments?: Array<ManualAssignment> | null;
  /** RANDOM / BALANCED: target rooms (default: every CREATED/OPEN room). */
  breakoutIds?: Array<string> | null;
  /** Include OWNER/HOST/CO_HOST participants (default false). */
  includeHosts?: boolean;
  /** Include VIEWER participants (default true). */
  includeViewers?: boolean | null;
  mode?: AssignmentMode | null;
  /** Only distribute participants without an assignment (default true). */
  onlyUnassigned?: boolean | null;
}


export type AssignmentMode = "MANUAL" | "RANDOM" | "BALANCED";


export interface AssignmentPair {
  breakoutId: string;
  participantId: string;
}


export interface AssignmentResult {
  assigned: Array<AssignmentPair>;
  enforcement: string;
  /** Eligible participants left unassigned (all rooms at capacity). */
  unassigned: Array<string>;
}


export interface AttendanceEntry {
  admissionState: AdmissionState;
  admittedAt?: string | null;
  attendanceSeconds: number;
  displayName: string;
  identityType: IdentityType;
  participantId: string;
  role: Role;
}

/**
 * Signed, short-lived, single-object read access (never persisted).
 * Conference attendance (Part J attendance contract). Attendance time is merged from
 * participant-session intervals, so multi-device presence is never double counted.
 */
export interface AttendanceReport {
  conferenceId: string;
  generatedAt: string;
  participants: Array<AttendanceEntry>;
  sessionCount: number;
}


export interface AuditProfile {
  /** Enhanced semantic audit (join, artefact access, consent, admin access). */
  enhanced: boolean;
}


export interface BackfillReportResponse {
  conferenceParticipantRowsFromLegacy: number;
  insertedConferences: number;
  insertedParticipants: number;
  insertedSessions: number;
  insertedSpaces: number;
  legacyConferenceSessions: number;
  legacyMeetingRows: number;
  legacyParticipantRows: number;
  mappedLegacyParticipantRows: number;
  mappedSpaces: number;
  mappingFailures: number;
  meetingSpaceRows: number;
  participantSessionRowsFromLegacy: number;
  unmappedMeetingIds: Array<string>;
  unmappedParticipantIds: Array<string>;
}


export interface BackstageResponse {
  /**
   * Request a fresh credential for the new context
   * (`POST /api/v2/conferences/{id}/media-credential`).
   */
  credentialRefreshRequired: boolean;
  inBackstage: boolean;
  mediaContext: MediaContextView;
  participantId: string;
}


export interface BanParticipantRequest {
  expiresAt?: string | null;
  reason?: string | null;
  reasonCode?: string | null;
  scope?: BanScope | null;
}


export type BanScope = "SPACE" | "CONFERENCE";


export type BanSubjectType = "USER" | "EXTERNAL" | "GUEST";


export interface BreakoutAnnouncementRequest {
  message: string;
}


export interface BreakoutListCreatedResponse {
  breakouts: Array<BreakoutView>;
}


export interface BreakoutListResponse {
  breakouts: Array<BreakoutView>;
  myContext?: MediaContextView | null;
}

/** Breakout recording metadata only — recording itself is Part G. */
export type BreakoutRecordingPolicy = "INHERIT_MAIN_POLICY" | "DISABLED" | "SEPARATE_ALLOWED";


export interface BreakoutResponse {
  breakout: BreakoutView;
  changed: boolean;
  providerEnforcement: ProviderEnforcement;
}


export type BreakoutStatus = "CREATED" | "OPEN" | "CLOSING" | "CLOSED";


export interface BreakoutView {
  capacity?: number | null;
  closedAt?: string | null;
  closingAt?: string | null;
  conferenceId: string;
  id: string;
  name: string;
  openedAt?: string | null;
  participantCount: number;
  /** Assigned participant ids — only for roles that manage breakouts. */
  participantIds?: Array<string> | null;
  position: number;
  recordingPolicy: string;
  status: BreakoutStatus;
  version: number;
}


export interface BulkAdmissionResponse {
  /** Transitioned by this request. */
  changed: Array<string>;
  /** In a state that does not allow this transition. */
  skipped: Array<SkippedParticipant>;
  /** Already in the requested state (idempotent retry). */
  unchanged: Array<string>;
}

/** A first-party business organization (the Organizations Service resource). */
export interface BusinessOrganization {
  archivedAt?: string | null;
  /** When the organization was created on CF Meet (never an incorporation date). */
  createdAt: string;
  createdBy?: string | null;
  /** SELF_SERVICE, PLATFORM_ADMIN or IMPORTED. */
  creationSource: string;
  id: string;
  /** ACTIVE, SUSPENDED or ARCHIVED. */
  lifecycleStatus: string;
  memberCount?: number | null;
  name: string;
  profile?: OrganizationProfile | null;
  /** NOT_REQUIRED, PENDING, IN_PROGRESS, READY or FAILED. */
  provisioningState: string;
  slug: string;
  updatedAt: string;
  version: number;
  viewer?: OrganizationViewer | null;
}

/** One membership of a first-party organization. */
export interface BusinessOrganizationMembership {
  createdAt: string;
  createdBy?: string | null;
  id: string;
  /** CREATOR, INVITATION, IMPORTED, OPERATOR or OWNERSHIP_TRANSFER. */
  joinedVia: string;
  organizationId: string;
  removedAt?: string | null;
  /** OWNER, ADMIN, MANAGER, MEMBER or GUEST. */
  role: string;
  /** ACTIVE, SUSPENDED or REMOVED. */
  status: string;
  updatedAt: string;
  userId: string;
  version: number;
}

/** A page of organizations (platform administrators). */
export interface BusinessOrganizationPage {
  items: Array<BusinessOrganization>;
  /** Pass as `cursor` for the next page; null on the last page. */
  nextCursor?: string | null;
}


export interface ChangePasswordRequest {
  confirm_password: string;
  current_password: string;
  new_password: string;
}


export interface ChangedResponse {
  changed: boolean;
}

/** Single chat message response */
export interface ChatMessageResponse {
  created_at: string;
  display_name: string;
  id: string;
  message: string;
  participant_id: string;
}

/** Response for listing chat messages */
export interface ChatMessagesListResponse {
  data: Array<ChatMessageResponse>;
  success: boolean;
}


export type ChatMigrationState = "LEGACY_PRIMARY" | "SHADOW_VALIDATION" | "MESSAGING_PRIMARY" | "LEGACY_RETIRED";

/** Chat-only subset of the collaboration policy. */
export interface ChatPolicyUpdateRequest {
  breakoutChatEnabled?: boolean | null;
  chatEnabled?: boolean | null;
  chatSendMode?: ChatSendMode | null;
  collaborationVersion: number;
  mainChatVisibleInBreakouts?: boolean | null;
  privateChatMode?: PrivateChatMode | null;
}


export type ChatSendMode = "EVERYONE" | "HOSTS_ONLY";


export interface CloseBreakoutRequest {
  /** Grace period before participants return (0 = immediate, max 600). */
  countdownSeconds?: number | null;
}


export interface CloseBreakoutsResponse {
  breakouts: Array<BreakoutView>;
  providerEnforcement: ProviderEnforcement;
}

/** Complete effective collaboration policy of a conference. */
export interface CollaborationPolicy {
  /** Show an AI-processing notice before join (no AI runs in Part F). */
  aiNotice: boolean;
  allowedReactions: Array<string>;
  /** Conference-wide microphone lock for every role without `participant.mute`. */
  audioLocked: boolean;
  breakoutChatEnabled: boolean;
  breakoutRecordingPolicy: BreakoutRecordingPolicy;
  chatEnabled: boolean;
  chatSendMode: ChatSendMode;
  consentVersion: string;
  handRaiseEnabled: boolean;
  /** Whether participants inside a breakout still see (read-only) main chat. */
  mainChatVisibleInBreakouts: boolean;
  maxBreakouts: number;
  maxSpotlights: number;
  pollsEnabled: boolean;
  privateChatMode: PrivateChatMode;
  qnaAnonymousAllowed: boolean;
  qnaEnabled: boolean;
  reactionsEnabled: boolean;
  /** Consents an attendee must grant before admission (pre-join contract). */
  requiredConsents: Array<ConsentKind>;
  screenSharePolicy: ScreenSharePolicy;
  /** Conference-wide camera lock for every role without `participant.mute`. */
  videoLocked: boolean;
}

/** Sparse override. Unknown fields are rejected. */
export interface CollaborationPolicyPatch {
  aiNotice?: boolean | null;
  allowedReactions?: Array<string> | null;
  audioLocked?: boolean | null;
  breakoutChatEnabled?: boolean | null;
  breakoutRecordingPolicy?: BreakoutRecordingPolicy | null;
  chatEnabled?: boolean | null;
  chatSendMode?: ChatSendMode | null;
  consentVersion?: string | null;
  handRaiseEnabled?: boolean | null;
  mainChatVisibleInBreakouts?: boolean | null;
  maxBreakouts?: number | null;
  maxSpotlights?: number | null;
  pollsEnabled?: boolean | null;
  privateChatMode?: PrivateChatMode | null;
  qnaAnonymousAllowed?: boolean | null;
  qnaEnabled?: boolean | null;
  reactionsEnabled?: boolean | null;
  requiredConsents?: Array<ConsentKind> | null;
  screenSharePolicy?: ScreenSharePolicy | null;
  videoLocked?: boolean | null;
}


export interface CollaborationPolicyResponse {
  collaborationVersion: number;
  conferenceId: string;
  policy: CollaborationPolicy;
  providerEnforcement: ProviderEnforcement;
}


export interface CollaborationPolicyUpdateRequest {
  changes: CollaborationPolicyPatch;
  /** Optimistic concurrency: the `collaborationVersion` the client last read. */
  collaborationVersion: number;
}

/** Canonical collaboration snapshot (REST recovery after realtime gaps). */
export interface CollaborationStateResponse {
  activeScreenShares: Array<string>;
  collaborationVersion: number;
  conferenceId: string;
  /** Realtime cursor this snapshot is at least as new as. */
  eventSequence: number;
  me?: MyCollaborationState | null;
  policy: CollaborationPolicy;
  raisedHands: Array<RaisedHand>;
  spotlights: Array<SpotlightView>;
}


export interface ConferenceActionResponse {
  /** False when the request was an idempotent no-op (e.g. already LIVE). */
  changed: boolean;
  conference: ConferenceResponse;
  providerEnforcement: ProviderEnforcement;
}


export type ConferenceKind = "INSTANT" | "SCHEDULED" | "RECURRING" | "LEGACY";


export interface ConferenceListResponse {
  items: Array<ConferenceResponse>;
}


export interface ConferenceQualityView {
  activeSessions: number;
  avgJitterMs?: number | null;
  avgPacketLossRatio?: number | null;
  avgRttMs?: number | null;
  conferenceId: string;
  degradedSessions: number;
  healthState: string;
  healthySessions: number;
  reconnects: number;
  unhealthySessions: number;
}


export interface ConferenceResponse {
  endedAt?: string | null;
  endingRequestedAt?: string | null;
  id: string;
  isException?: boolean | null;
  isLegacy?: boolean | null;
  isLocked?: boolean | null;
  kind?: ConferenceKind | null;
  lockedAt?: string | null;
  meetingSpaceId: string;
  occurrenceKey?: string | null;
  originalStartAt?: string | null;
  /**
   * Part G: recording indicator (`OFF` / `REQUESTED` / `RECORDING` / `STOPPING`) —
   * the authoritative recovery source after a reconnect.
   */
  recordingState?: string | null;
  scheduledEndAt?: string | null;
  scheduledStartAt?: string | null;
  startedAt?: string | null;
  status: ConferenceStatus;
  terminationReason?: TerminationReason | null;
  /** Part G: live transcription indicator. */
  transcriptionState?: string | null;
  version?: number | null;
  view: ResourceView;
}


export type ConferenceStatus = "CREATED" | "PREPARING" | "WAITING" | "LIVE" | "ENDING" | "ENDED" | "CANCELLED" | "FAILED";

/** Minimal conference info included in space responses. */
export interface ConferenceSummary {
  endedAt?: string | null;
  id: string;
  scheduledEndAt?: string | null;
  scheduledStartAt?: string | null;
  startedAt?: string | null;
  status: ConferenceStatus;
}


export type ConsentDecision = "ACCEPTED" | "DECLINED" | "WITHDRAWN";


export interface ConsentDecisionInput {
  consentType: ConsentKind;
  decision: ConsentDecision;
}


export interface ConsentGrant {
  type: ConsentKind;
  version: string;
}

/** Explicit pre-join consent kinds (`meeting_consents.consent_type`). */
export type ConsentKind = "RECORDING" | "TRANSCRIPTION" | "AI_PROCESSING" | "DATA_SHARING" | "HEALTHCARE_TERMS" | "PRIVACY_NOTICE";


export type ConsentMode = "NOTICE_ONLY" | "EXPLICIT_CONSENT_REQUIRED";


export interface ConsentRequirement {
  /** The processing is currently active in this conference. */
  active: boolean;
  consentType: ConsentKind;
  consentVersion: string;
  decidedAt?: string | null;
  decision?: ConsentDecision | null;
  mode: ConsentMode;
  /** Affirmative consent is required before this processing runs. */
  required: boolean;
}


export interface ConsentRequirementsResponse {
  conferenceId: string;
  /** Recording notice is always shown while capture is active. */
  recordingNotice: boolean;
  requirements: Array<ConsentRequirement>;
}

/** API response wrapper for create API key */
export interface CreateApiKeyApiResponse {
  data: CreateApiKeyResponse;
  success: boolean;
}

/** Request to create a new API key (no user-configurable fields - system sets defaults) */
export type CreateApiKeyRequest = Record<string, unknown>;

/** Response when creating an API key (includes the full key - only shown once) */
export interface CreateApiKeyResponse {
  /** The full API key - only returned on creation, never retrievable again */
  api_key: string;
  created_at: string;
  expires_at: string;
  id: string;
  status: string;
  usage_limit: number;
  used_count: number;
}


export interface CreateBreakoutsRequest {
  capacity?: number | null;
  /** Number of automatically numbered rooms ("Room N"). */
  count?: number | null;
  /** Named rooms. Mutually exclusive with `count`. */
  names?: Array<string> | null;
  recordingPolicy?: BreakoutRecordingPolicy | null;
}

/** `POST /api/v2/organizations`. */
export interface CreateBusinessOrganizationRequest {
  /** 1–200 characters. */
  name: string;
  profile?: OrganizationProfile | null;
  /** 3–63 lowercase letters, digits and `-`, starting and ending with a letter or digit. */
  slug: string;
}


export interface CreateConferenceRequest {
  scheduledEndAt?: string | null;
  scheduledStartAt?: string | null;
}


export interface CreateInvitationRequest {
  /** Restrict the invitation to one occurrence. */
  conferenceId?: string | null;
  /** Default 168 (7 days); capped by server policy. */
  expiresInHours?: number | null;
  inviteeEmail?: string | null;
  role?: Role | null;
  /**
   * Blueprint Section 11: also email the invitation link to `inviteeEmail` (required
   * with this flag). The result is reported in `emailDelivery`. Default false.
   */
  sendEmail?: boolean | null;
  useLimit?: number | null;
}


export interface CreateInvitationResponse {
  emailDelivery?: EmailDelivery | null;
  invitation: InvitationResponse;
  /** Raw bearer token. Returned exactly once; only its hash is stored. */
  token: string;
}

/** Request to create a new meeting */
export interface CreateMeetingRequest {
  /** End time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS" */
  end_time?: string | null;
  is_private?: boolean | null;
  metadata?: Record<string, unknown> | null;
  /** Start time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS" */
  start_time: string;
  /** IANA timezone of the user (e.g., "Africa/Lagos", "America/New_York") */
  timezone: string;
  title: string;
}


export interface CreateMeetingSpaceRequest {
  accessConfig?: AccessConfig | null;
  accessPolicy?: AccessPolicy | null;
  collaborationPolicy?: CollaborationPolicyPatch | null;
  defaultRole?: Role | null;
  description?: string | null;
  interactiveCapacity?: number | null;
  meetingType?: MeetingType | null;
  /** Must be an organization the caller verifiably belongs to. */
  organizationId?: string | null;
  /** Plaintext passcode; only a bcrypt hash is stored and it is never returned. */
  passcode?: string | null;
  recordingPolicy?: MediaCapturePolicy | null;
  retentionPolicyRef?: string | null;
  rolePolicy?: RolePolicy | null;
  schedulePolicy?: SchedulePolicy | null;
  securityProfile?: SecurityProfile | null;
  /** IANA timezone, e.g. `Africa/Lagos`. Defaults to `UTC`. */
  timezone?: string | null;
  title: string;
  transcriptionPolicy?: MediaCapturePolicy | null;
  viewerCapacity?: number | null;
  waitingRoomPolicy?: WaitingRoomPolicy | null;
}

/** `POST /api/v2/organizations/{id}/invitations`. */
export interface CreateOrganizationInvitationRequest {
  /** The invitee's email address; only an account with this verified email can accept. */
  email: string;
  /** ADMIN, MANAGER, MEMBER or GUEST. */
  role: string;
  /**
   * Blueprint Section 11: also email the invitation link to `email` (default false).
   * Handled by this relay; the outcome is reported in `emailDelivery`.
   */
  sendEmail?: boolean | null;
}


export interface CreateOrganizationRequest {
  name: string;
  ownerUserId?: string | null;
  /** Lowercase letters, digits and '-'. */
  slug: string;
}


export interface CreatePollRequest {
  anonymous?: boolean;
  questions: Array<PollQuestionInput>;
  quizMode?: boolean;
  resultsVisibility?: ResultsVisibility | null;
  title: string;
}


export interface CreateWebhookApiResponse {
  data: CreateWebhookResponse;
  /**
   * Deprecation notice: nothing registered here is delivered; use Developer Platform
   * webhooks.
   */
  notice: string;
  success: boolean;
}


export interface CreateWebhookRequest {
  endpoint_url: string;
  /** Event type: meeting_start, meeting_end, participant_join, participant_leave */
  event_type: string;
}


export interface CreateWebhookResponse {
  created_at: string;
  /**
   * Always `NOT_DELIVERED`: legacy meeting webhooks are never delivered (deprecated;
   * use Developer Platform webhooks).
   */
  delivery: string;
  endpoint_url: string;
  event_type: string;
  id: string;
  /**
   * Generated secret, returned only on creation. No event is ever signed with it,
   * because legacy meeting webhooks are never delivered.
   */
  secret: string;
  status: string;
  updated_at: string;
  user_id: string;
}


export interface CustomRegistrationField {
  /** `[a-z][a-z0-9_]{0,31}` */
  key: string;
  label: string;
  maxLength: number;
  required: boolean;
}


export interface DataSharingProfile {
  allowed: boolean;
  consentRequired: boolean;
}

/** Optional body of POST /api/v1/users/me/deactivate (ACC-026). */
export interface DeactivateAccountRequest {
  /**
   * The account's current password. Required unless the caller signed in within the
   * re-authentication window (default 10 minutes); accounts without a password (Google
   * sign-in) re-authenticate by signing in again.
   */
  current_password?: string | null;
}


export interface DenyRequest {
  all?: boolean;
  /** Administrative note — stored, never shown to the participant. */
  internalNote?: string | null;
  /** Shown to the denied participant. */
  message?: string | null;
  participantIds?: Array<string>;
  reasonCode?: string | null;
}

/** Detailed participant response with media states */
export interface DetailedParticipantResponse {
  display_name: string;
  is_muted: boolean;
  is_screen_sharing: boolean;
  is_video_on: boolean;
  join_time: string;
  leave_time?: string | null;
  meeting_id: string;
  participant_id: string;
  role: string;
  status: string;
  user_id?: string | null;
}


export interface DuplicateMeetingSpaceRequest {
  title?: string | null;
}

/** What happened to a requested invitation email. */
export type EmailDelivery = "DISABLED" | "ACCEPTED" | "FAILED";

/** API response wrapper for end meeting */
export interface EndMeetingApiResponse {
  data: EndMeetingResponse;
  success: boolean;
}

/** Response for ending a meeting */
export interface EndMeetingResponse {
  status: string;
}


export interface ErrorBody {
  code: ErrorCode;
  /** Safe, human-readable message (never SQL, provider bodies, secrets or paths). */
  message: string;
  /** Server-generated correlation ID (also in the `x-request-id` response header). */
  requestId?: string | null;
  /** Present on `RATE_LIMITED` only. */
  retryAfterSeconds?: number | null;
}

/** Stable, semantic public error codes for the Phase 2 meeting domain (spec v1.1 §5). */
export type ErrorCode = "MEETING_NOT_FOUND" | "SESSION_NOT_FOUND" | "MEETING_ACCESS_DENIED" | "MEETING_PASSCODE_REQUIRED" | "MEETING_PASSCODE_INVALID" | "MEETING_VERSION_CONFLICT" | "MEETING_CAPACITY_REACHED" | "INVITATION_INVALID" | "INVITATION_EXPIRED" | "CONFERENCE_NOT_FOUND" | "CONFERENCE_NOT_LIVE" | "CONFERENCE_LOCKED" | "CONFERENCE_STATE_CONFLICT" | "WAITING_ROOM_REQUIRED" | "PARTICIPANT_NOT_FOUND" | "PARTICIPANT_BANNED" | "PARTICIPANT_STATE_CONFLICT" | "PERMISSION_DENIED" | "AUTHENTICATION_REQUIRED" | "VALIDATION_ERROR" | "RATE_LIMITED" | "FEATURE_DISABLED" | "RTC_PROVIDER_UNAVAILABLE" | "RECORDING_NOT_ALLOWED" | "REVOCATION_CHECK_UNAVAILABLE" | "MEDIA_PERMISSION_DENIED" | "MEDIA_LOCKED" | "SCREEN_SHARE_NOT_ALLOWED" | "SCREEN_SHARE_NOT_ACTIVE" | "CHAT_DISABLED" | "PRIVATE_CHAT_DISABLED" | "CHAT_PARTICIPANT_RESTRICTED" | "REACTIONS_DISABLED" | "HAND_RAISE_DISABLED" | "BREAKOUT_NOT_FOUND" | "BREAKOUT_CLOSED" | "BREAKOUT_ASSIGNMENT_CONFLICT" | "POLL_NOT_FOUND" | "POLL_NOT_OPEN" | "POLL_ALREADY_RESPONDED" | "POLL_RESPONSE_INVALID" | "POLL_RESULTS_NOT_VISIBLE" | "QUESTION_NOT_FOUND" | "QNA_DISABLED" | "POLLS_DISABLED" | "MESSAGE_NOT_FOUND" | "CONSENT_REQUIRED" | "RECORDING_DISABLED" | "RECORDING_ALREADY_ACTIVE" | "RECORDING_NOT_ACTIVE" | "RECORDING_STATE_CONFLICT" | "RECORDING_PROVIDER_UNAVAILABLE" | "RECORDING_NOT_FOUND" | "CONSENT_DECLINED" | "CONSENT_POLICY_CONFLICT" | "ARTIFACT_NOT_FOUND" | "ARTIFACT_NOT_READY" | "ARTIFACT_DELETED" | "ARTIFACT_ACCESS_DENIED" | "ARTIFACT_STORAGE_UNAVAILABLE" | "ARTIFACT_INTEGRITY_FAILED" | "TRANSCRIPTION_DISABLED" | "TRANSCRIPTION_NOT_ALLOWED" | "TRANSCRIPTION_PROVIDER_UNAVAILABLE" | "TRANSCRIPTION_NOT_FOUND" | "INTELLIGENCE_DISABLED" | "INTELLIGENCE_NOT_ALLOWED" | "INTELLIGENCE_PROVIDER_UNAVAILABLE" | "REGISTRATION_DISABLED" | "REGISTRATION_CLOSED" | "REGISTRATION_NOT_FOUND" | "ADMISSION_TOKEN_INVALID" | "BACKSTAGE_ACCESS_DENIED" | "PROFILE_POLICY_DENIED" | "STAGE_CAPACITY_REACHED" | "QUALITY_SAMPLE_INVALID" | "QUALITY_RATE_LIMITED" | "OPERATIONS_ACCESS_DENIED" | "REQUEST_TOO_LARGE" | "ENDPOINT_NOT_FOUND" | "METHOD_NOT_ALLOWED" | "LEGACY_WRITE_DISABLED" | "CHAT_UNAVAILABLE" | "CHAT_MIGRATION_REFUSED" | "SERVICE_UNAVAILABLE" | "ORGANIZATION_NOT_FOUND" | "MEMBERSHIP_REQUIRED" | "ORGANIZATION_SUSPENDED" | "ORGANIZATION_ARCHIVED" | "LAST_OWNER" | "VERSION_CONFLICT" | "SLUG_TAKEN" | "INVITE_EXPIRED" | "IDEMPOTENCY_CONFLICT" | "PROVISIONING_PENDING" | "INVITATION_REQUIRED" | "ORGANIZATION_POLICY_RESTRICTED" | "ORGANIZATION_CALLING_DISABLED" | "USERNAME_TAKEN" | "REAUTHENTICATION_REQUIRED" | "EMAIL_VERIFICATION_REQUIRED" | "DATABASE_ERROR" | "INTERNAL_ERROR";

/** OpenAPI description of the public error envelope. */
export interface ErrorEnvelope {
  error: ErrorBody;
  success: boolean;
}


export interface EventReplayResponse {
  canonical: string;
  /** Cursor to resume from. */
  cursor: number;
  events: Array<RealtimeEnvelope>;
  /** True when the cursor cannot be served: refetch `canonical` and resume from `cursor`. */
  resyncRequired: boolean;
}


export interface ExportProfile {
  attendanceReportAllowed: boolean;
  chatExportAllowed: boolean;
  pollReportAllowed: boolean;
  retentionDays: number;
}


export type FailureCategoryDto = "PERMISSION_DENIED" | "PROVIDER_UNAVAILABLE" | "TRANSPORT" | "UNKNOWN";


export type FailureKind = "AUDIO_PUBLISH" | "VIDEO_PUBLISH" | "SCREEN_SHARE_PUBLISH" | "SUBSCRIBE" | "JOIN_MEDIA";


export type FieldRequirement = "REQUIRED" | "OPTIONAL" | "DISABLED";


export interface ForgotPasswordRequest {
  email: string;
}


export interface GenerateReportRequest {
  artifactType: ArtifactType;
}

/**
 * One-time Google sign-in exchange (ACC-005): the callback redirect carries only a
 * short-lived, single-use `exchange_code`, never tokens.
 */
export interface GoogleExchangeRequest {
  exchange_code: string;
}


export interface HandRaiseResponse {
  changed: boolean;
  participantId: string;
  raised: boolean;
  raisedAt?: string | null;
}


export type IdentityType = "REGISTERED_USER" | "INVITED_EXTERNAL" | "GUEST" | "PHONE" | "SERVICE" | "BOT";

/** API response wrapper for instant meeting */
export interface InstantMeetingApiResponse {
  data: InstantMeetingResponse;
  success: boolean;
}


export interface InstantMeetingRequest {
  accessPolicy?: AccessPolicy | null;
  passcode?: string | null;
  timezone?: string | null;
  title?: string | null;
  waitingRoomPolicy?: WaitingRoomPolicy | null;
}


export interface InstantMeetingResponse {
  conference: ConferenceResponse;
  meetingSpace: MeetingSpaceResponse;
}


export type IntelligenceKind = "SUMMARY" | "ACTION_ITEMS";


export interface IntelligenceProfile {
  consentRequired: boolean;
  location: ProcessingLocation;
  retentionDays: number;
}


export interface IntelligenceRequest {
  kind: IntelligenceKind;
  /** READY transcript artefact of this conference. */
  sourceArtifactId: string;
}


export interface InvitationResponse {
  conferenceId?: string | null;
  createdAt: string;
  expiresAt: string;
  id: string;
  inviteeEmail?: string | null;
  meetingSpaceId: string;
  requestedRole: Role;
  status: InvitationStatus;
  usageCount: number;
  useLimit?: number | null;
}


export type InvitationStatus = "ACTIVE" | "REVOKED";


export interface JoinConferenceRequest {
  /** Part F: explicit consents granted before admission (see the pre-join contract). */
  consents?: Array<ConsentGrant> | null;
  /** Presentation name only — never identity. Required for guests. */
  displayName?: string | null;
  /**
   * Opaque context forwarded to a server-side access authority (e.g. a HealthNet
   * appointment token). Never treated as proof by the meeting service itself.
   */
  externalContext?: string | null;
  invitationToken?: string | null;
  passcode?: string | null;
  /** Reconnect: supersede this earlier session of the same participant. */
  previousSessionId?: string | null;
}


export interface JoinConferenceResponse {
  conference: JoinConferenceSummary;
  mediaContext: MediaContextView;
  participant: ParticipantResponse;
  participantSession?: ParticipantSessionResponse | null;
  /**
   * Server-issued participant credential for non-registered actors (send as
   * `X-Participant-Token`). Returned only when newly issued.
   */
  participantToken?: string | null;
  permissions: JoinPermissions;
  policy: JoinPolicy;
  realtime: JoinRealtimeInfo;
  rtc?: RtcCredentialResponse | null;
  rtcStatus: RtcStatus;
}


export interface JoinConferenceSummary {
  id: string;
  isLocked: boolean;
  meetingSpaceId: string;
  status: ConferenceStatus;
}

/** API response wrapper for join meeting */
export interface JoinMeetingApiResponse {
  data: JoinMeetingResponse;
  success: boolean;
}

/** Request to join a meeting */
export interface JoinMeetingRequest {
  display_name: string;
  /**
   * Optional, compatibility only. Registered identity is taken from the
   * `Authorization: Bearer <access token>` header; if present, this value must equal
   * the authenticated user's id (401 without a token, 403 on mismatch). Guests omit it
   * and resume with their guest access token.
   */
  user_id?: string | null;
}

/** Response for joining a meeting (includes join token) */
export interface JoinMeetingResponse {
  /** JWT Access Token (issued for guests) */
  access_token?: string | null;
  /** Short-lived token for WebSocket/RTC session */
  join_token: string;
  /** LiveKit server URL */
  livekit_url: string;
  meeting_id: string;
  participant_id: string;
  /** JWT Refresh Token (issued for guests) */
  refresh_token?: string | null;
  role: string;
  /** Room name for LiveKit */
  room_name: string;
}


export interface JoinPermissions {
  audioPublish: boolean;
  dataPublish: boolean;
  screenSharePublish: boolean;
  subscribe: boolean;
  videoPublish: boolean;
}


export interface JoinPolicy {
  recordingActive: boolean;
  transcriptionActive: boolean;
}

/**
 * Realtime subscription and recovery resources for a joined (or waiting) participant.
 * Paths are relative to the API origin; authorization is re-evaluated on every call.
 */
export interface JoinRealtimeInfo {
  /** Canonical conference state — the primary recovery resource. */
  conferencePath: string;
  /** Whether the conference WebSocket stream is available (`FEATURE_CONFERENCE_REALTIME_ENABLED`). */
  enabled: boolean;
  /** REST replay of missed events (`?after=<sequence>`; present when `enabled`). */
  eventsPath?: string | null;
  /** The caller's own participant state (admission decision fallback when polling). */
  participantPath: string;
  /** WebSocket stream (`?ticket=…&cursor=…`; present when `enabled`). */
  streamPath?: string | null;
  /** `POST` → one-shot WebSocket ticket (present when `enabled`). */
  ticketPath?: string | null;
}


export interface LeaveConferenceRequest {
  /** Close one session; omitted = close all of the caller's sessions. */
  participantSessionId?: string | null;
}


export interface LeaveConferenceResponse {
  closedSessions: number;
}

/** Response for listing API keys */
export interface ListApiKeysResponse {
  data: Array<ApiKeyResponse>;
  success: boolean;
}


export interface ListConferencesQuery {
  from?: string | null;
  to?: string | null;
}

/**
 * `GET /api/v2/meeting-spaces` paging (both optional; omitting both keeps the legacy
 * complete list).
 */
export interface ListMeetingSpacesQuery {
  /** Opaque cursor from a previous page's `nextCursor`. */
  cursor?: string | null;
  /** Page size, 1-200 (default 50 when `cursor` is sent without `limit`). */
  limit?: number | null;
}


export interface LoginRequest {
  email: string;
  password: string;
}


export interface LoginResponse {
  access_token: string;
  refresh_token: string;
  user: UserInfo;
}


export interface ManualAssignment {
  breakoutId: string;
  participantId: string;
}


export type MediaCapturePolicy = "DISABLED" | "HOST_OPTIONAL" | "AUTO_START" | "ORGANIZATION_CONTROLLED";


export interface MediaContextResponse {
  changed: boolean;
  mediaContext: MediaContextView;
  participantId?: string | null;
  providerEnforcement: ProviderEnforcement;
}


export interface MediaContextView {
  breakoutId?: string | null;
  /** MAIN, BREAKOUT or BACKSTAGE. */
  kind: string;
  visit: boolean;
}


export interface MediaCredentialRequest {
  participantSessionId: string;
}


export interface MediaCredentialResponse {
  mediaContext: MediaContextView;
  permissions: JoinPermissions;
  rtc: RtcCredentialResponse;
}


export interface MediaFailure {
  category: FailureCategoryDto;
  kind: FailureKind;
}


export type MediaHealth = "OK" | "DEGRADED" | "FAILED";


export interface MediaLockResponse {
  audioLocked: boolean;
  changed: boolean;
  participantId: string;
  permissions: JoinPermissions;
  providerEnforcement: ProviderEnforcement;
  screenShareLocked: boolean;
  videoLocked: boolean;
}

/** Media permission projection handed to the RTC provider (Part E enforces it). */
export interface MediaPermissions {
  audioPublish: boolean;
  dataPublish: boolean;
  moderateMedia: boolean;
  record: boolean;
  screenSharePublish: boolean;
  subscribe: boolean;
  videoPublish: boolean;
}

/** Response for media state update */
export interface MediaStateResponse {
  is_muted: boolean;
  is_screen_sharing: boolean;
  is_video_on: boolean;
  participant_id: string;
  success: boolean;
}

/** API response wrapper for meeting */
export interface MeetingApiResponse {
  data: MeetingResponse;
  success: boolean;
}

/** Pagination metadata */
export interface MeetingPaginationMeta {
  limit: number;
  page: number;
  total: number;
  total_pages: number;
}

/** The specialized mode a space resolves to. */
export type MeetingProfile = "STANDARD" | "WEBINAR" | "TOWN_HALL" | "HEALTHCARE" | "HIGH_SECURITY";

/** Meeting response structure */
export interface MeetingResponse {
  created_at: string;
  /** End time in UTC. Format: "2026-01-25T15:00:00Z" */
  end_time?: string | null;
  host_id: string;
  id: string;
  is_private: boolean;
  /** Join URL for the hosted UI (e.g., https://meet.cosmicforge.com/join/ABCD1234) */
  join_url: string;
  meeting_identifier: string;
  metadata?: Record<string, unknown> | null;
  /** Start time in UTC. Format: "2026-01-25T14:00:00Z" */
  start_time: string;
  status: string;
  title: string;
  updated_at: string;
}


export interface MeetingSpaceListResponse {
  items: Array<MeetingSpaceResponse>;
  /**
   * Present only on paginated requests (`limit` or `cursor` sent) when another page
   * exists: pass it back as `cursor`.
   */
  nextCursor?: string | null;
}


export interface MeetingSpaceResponse {
  accessConfig?: AccessConfig | null;
  accessPolicy: AccessPolicy;
  archivedAt?: string | null;
  collaborationPolicy?: CollaborationPolicyPatch | null;
  createdAt?: string | null;
  defaultRole?: Role | null;
  description?: string | null;
  id: string;
  interactiveCapacity?: number | null;
  isLegacy: boolean;
  latestConference?: ConferenceSummary | null;
  meetingCode: string;
  meetingType: MeetingType;
  organizationId?: string | null;
  ownerUserId?: string | null;
  recordingPolicy?: MediaCapturePolicy | null;
  requiresPasscode: boolean;
  retentionPolicyRef?: string | null;
  rolePolicy?: RolePolicy | null;
  schedulePolicy?: SchedulePolicy | null;
  securityProfile?: SecurityProfile | null;
  status: SpaceStatus;
  timezone?: string | null;
  title?: string | null;
  transcriptionPolicy?: MediaCapturePolicy | null;
  updatedAt?: string | null;
  version?: number | null;
  view: ResourceView;
  viewerCapacity?: number | null;
  viewerRole?: Role | null;
  waitingRoomPolicy?: WaitingRoomPolicy | null;
}


export type MeetingType = "STANDARD" | "HEALTHCARE" | "WEBINAR" | "TOWN_HALL" | "TRAINING";


export interface MemberListResponse {
  items: Array<MemberResponse>;
}


export interface MemberResponse {
  createdAt: string;
  email?: string | null;
  id: string;
  meetingSpaceId: string;
  principalType: PrincipalType;
  revokedAt?: string | null;
  role: Role;
  status: MemberStatus;
  updatedAt: string;
  userId?: string | null;
  version: number;
}


export type MemberStatus = "ACTIVE" | "REVOKED";


export type MessageKind = "MESSAGE" | "ANNOUNCEMENT" | "BREAKOUT_ANNOUNCEMENT";


export interface MessageListQuery {
  /** Cursor from a previous page (`nextCursor`). */
  before?: string | null;
  breakoutId?: string | null;
  limit?: number | null;
  /** PRIVATE: the other participant of the conversation. */
  peerId?: string | null;
  /** MAIN (default), BREAKOUT or PRIVATE. */
  scope?: string | null;
}


export interface MessagePage {
  messages: Array<MessageView>;
  /** Pass as `before` to fetch older messages; absent at the end. */
  nextCursor?: string | null;
}


export interface MessageResponse {
  message: string;
}


export interface MessageView {
  /** `null` once deleted by moderation. */
  body?: string | null;
  breakoutId?: string | null;
  conferenceId: string;
  createdAt: string;
  deleted: boolean;
  id: string;
  kind: MessageKind;
  recipientParticipantId?: string | null;
  senderDisplayName: string;
  senderParticipantId: string;
}


export interface MigrationStatusView {
  changedAt: string;
  changedBy: string;
  latestParityRun?: ParityRunView | null;
  linkedConferences: number;
  /** Legacy messages (every scope) of linked conferences not yet mirrored. */
  pendingMirror: number;
  /** Messaging-native messages not yet copied back after a rollback. */
  pendingRollbackCopies: number;
  pendingSessionRevocations: number;
  /** What is migrated: every meeting chat scope. */
  scope: unknown;
  state: ChatMigrationState;
  transitions: Array<TransitionView>;
  version: number;
}


export interface ModerationResponse {
  banId?: string | null;
  participant: ParticipantResponse;
  providerEnforcement: ProviderEnforcement;
}


export interface MoveParticipantRequest {
  participantId: string;
  /** Target breakout; null = back to the main conference. */
  toBreakoutId?: string | null;
}


export interface MuteRequest {
  trackKind: TrackKind;
}


export interface MuteResponse {
  /** Published tracks the provider muted. */
  mutedTracks: number;
  participantId: string;
  /** Connected device sessions examined. */
  sessions: number;
  trackKind: TrackKind;
}


export interface MyCollaborationState {
  audioLocked: boolean;
  chatRestricted: boolean;
  handRaisedAt?: string | null;
  mediaContext: MediaContextView;
  participantId: string;
  permissions: JoinPermissions;
  role: Role;
  screenShareLocked: boolean;
  videoLocked: boolean;
}


export interface MyOrganizationResponse {
  organization: OrganizationResponse;
  /**
   * Once the Organizations Service is authoritative: the caller's full role (OWNER,
   * ADMIN, MANAGER or MEMBER); `role` then shows MANAGER as MEMBER.
   */
  organizationRole?: string | null;
  role: OrganizationRole;
}


export type NetworkType = "WIFI" | "CELLULAR" | "ETHERNET" | "UNKNOWN";

/** An organization invitation. The raw `token` is returned once, at creation only. */
export interface OrganizationInvitation {
  createdAt: string;
  decidedAt?: string | null;
  emailDelivery?: EmailDelivery | null;
  expiresAt: string;
  id: string;
  invitedBy?: string | null;
  /** Masked invitee email, e.g. `a***@example.com`. */
  inviteeEmailHint: string;
  organizationId: string;
  role: string;
  /** PENDING, ACCEPTED, DECLINED, REVOKED or EXPIRED. */
  status: string;
  /** One-time invitation token (creation response only; never in URLs or logs). */
  token?: string | null;
}

/** `POST /api/v2/organization-invitations/accept` and `/decline`. */
export interface OrganizationInvitationTokenRequest {
  /** The invitation token received by the invitee. */
  token: string;
}

/**
 * Optional legal registration of an organization (self-declared until an operator
 * verifies it; never the CF Meet creation time).
 */
export interface OrganizationLegalRegistration {
  /** `YYYY-MM-DD`. */
  incorporatedOn?: string | null;
  /** Up to 200 characters. */
  registeredName?: string | null;
  /** Up to 100 characters. */
  registrationNumber?: string | null;
  /** `SELF_DECLARED` or `VERIFIED` (read-only; set by an operator). */
  verification?: string | null;
}

/** Suspend, reactivate or archive an organization. */
export interface OrganizationLifecycleRequest {
  /** Optional organization `version` the change is based on. */
  expectedVersion?: number | null;
}


export interface OrganizationMemberResponse {
  createdAt: string;
  id: string;
  organizationId: string;
  /**
   * Once the Organizations Service is authoritative: the member's full role (OWNER,
   * ADMIN, MANAGER, MEMBER or GUEST); `role` then shows MANAGER and GUEST as MEMBER.
   */
  organizationRole?: string | null;
  role: OrganizationRole;
  status: OrganizationMemberStatus;
  updatedAt: string;
  userId: string;
  /**
   * Once the Organizations Service is authoritative: the membership version, used as
   * `expectedVersion` by `PATCH /api/v2/organizations/{id}/members/{userId}`.
   */
  version?: number | null;
}


export type OrganizationMemberStatus = "ACTIVE" | "SUSPENDED" | "REMOVED";

/** `POST /api/v2/organizations/{id}/ownership-transfer`. */
export interface OrganizationOwnershipTransferRequest {
  /** The organization `version` the transfer is based on. */
  expectedVersion: number;
  /** The membership that becomes OWNER. */
  toMembershipId: string;
}

/** Organization profile. On update, an omitted field is unchanged and `null` clears it. */
export interface OrganizationProfile {
  /** Up to 120 characters. */
  city?: string | null;
  /** ISO 3166-1 alpha-2 country code. */
  countryCode?: string | null;
  /** Up to 2000 characters. */
  description?: string | null;
  /** Up to 60 characters. */
  industry?: string | null;
  legalRegistration?: OrganizationLegalRegistration | null;
  /** Up to 500 characters. */
  logoRef?: string | null;
  /** Up to 500 characters. */
  operatingAddress?: string | null;
  /** Up to 60 characters. */
  organizationType?: string | null;
  /** Up to 254 characters. */
  publicContactEmail?: string | null;
}


export interface OrganizationResponse {
  createdAt: string;
  id: string;
  name: string;
  slug: string;
  status: OrganizationStatus;
}


export type OrganizationRole = "OWNER" | "ADMIN" | "MEMBER";


export type OrganizationStatus = "ACTIVE" | "SUSPENDED";

/** The caller's own membership, as seen on an organization. */
export interface OrganizationViewer {
  /** Capabilities the caller holds in this organization (default deny). */
  capabilities: Array<string>;
  membershipId: string;
  /** OWNER, ADMIN, MANAGER, MEMBER or GUEST. */
  role: string;
  /** ACTIVE or SUSPENDED. */
  status: string;
}


export interface OwnerTransferRequest {
  newOwnerUserId: string;
  /** Keep the previous owner as a HOST member (default true). */
  retainPreviousOwner?: boolean;
  version: number;
}

/** Paginated response for meetings list */
export interface PaginatedMeetingResponse {
  data: Array<MeetingResponse>;
  meta: MeetingPaginationMeta;
  success: boolean;
}


export interface ParityRunView {
  conferencesChecked: number;
  finishedAt?: string | null;
  id: string;
  messagesChecked: number;
  mismatches: number;
  startedAt: string;
  status: string;
}

/** Response for participant action (kick, admit, deny) */
export interface ParticipantActionResponse {
  message: string;
  success: boolean;
}


export interface ParticipantResponse {
  admissionState: AdmissionState;
  admittedAt?: string | null;
  conferenceId: string;
  /** User-visible denial message (internal notes are never exposed). */
  denialMessage?: string | null;
  denialReasonCode?: string | null;
  displayName: string;
  id: string;
  identityType: IdentityType;
  requestedAt: string;
  role: Role;
  version: number;
}


export interface ParticipantSessionResponse {
  expiresAt?: string | null;
  id: string;
}

/** Response for listing participants */
export interface ParticipantsListResponse {
  data: Array<DetailedParticipantResponse>;
  success: boolean;
}

/**
 * Phone binding is provider-gated (ADR 0056 §8): `PROVIDER_BLOCKED` until an SMS or
 * verification provider is approved, and never required.
 */
export interface PhoneVerificationStatus {
  required: boolean;
  status: string;
}

/** Validated answer, as stored. */
export interface PollAnswer {
  optionIds?: Array<string> | null;
  ranking?: Array<string> | null;
  rating?: number | null;
  text?: string | null;
}


export interface PollAnswerInput {
  optionIds?: Array<string> | null;
  questionId: string;
  ranking?: Array<string> | null;
  rating?: number | null;
  text?: string | null;
}


export interface PollListResponse {
  polls: Array<PollView>;
}


export interface PollOption {
  id: string;
  label: string;
}

/** Stored, validated question. */
export interface PollQuestion {
  /** Option ids (choice/ranking) or rating value as string. Stripped from public views. */
  correct?: Array<string> | null;
  id: string;
  maxLength?: number | null;
  maxSelections?: number | null;
  minSelections?: number | null;
  options?: Array<PollOption>;
  points?: number | null;
  prompt: string;
  ratingMax?: number | null;
  ratingMin?: number | null;
  required: boolean;
  type: PollQuestionType;
}


export interface PollQuestionInput {
  /**
   * Quiz: correct option indexes (choice), the exact order (ranking) or `[value]`
   * (rating). Never returned before the results state allows it.
   */
  correct?: Array<number> | null;
  maxLength?: number | null;
  maxSelections?: number | null;
  minSelections?: number | null;
  /** Option labels (choice / ranking). Ids `o1…oN` are assigned by the server. */
  options?: Array<string> | null;
  points?: number | null;
  prompt: string;
  ratingMax?: number | null;
  ratingMin?: number | null;
  required?: boolean;
  type: PollQuestionType;
}


export type PollQuestionType = "SINGLE_CHOICE" | "MULTIPLE_CHOICE" | "SHORT_TEXT" | "LONG_TEXT" | "RATING" | "RANKING";


export interface PollResponseReceipt {
  /** true when this request was a retry of an already-recorded ballot. */
  idempotentReplay: boolean;
  maxScore?: number | null;
  pollId: string;
  recorded: boolean;
  /**
   * Quiz score, only where the results state already allows it (AFTER_VOTE,
   * non-anonymous).
   */
  score?: number | null;
}


export interface PollResults {
  anonymous: boolean;
  /** Quiz: average score and maximum (aggregate only). */
  averageScore?: number | null;
  maxScore?: number | null;
  /** The caller's own score (non-anonymous quiz, results visible). */
  myScore?: number | null;
  pollId: string;
  questions: Array<QuestionResult>;
  responseCount: number;
  status: PollStatus;
}


export type PollStatus = "DRAFT" | "OPEN" | "CLOSED" | "ARCHIVED";


export interface PollTransitionResponse {
  changed: boolean;
  poll: PollView;
}


export interface PollView {
  anonymous: boolean;
  archivedAt?: string | null;
  closedAt?: string | null;
  conferenceId: string;
  createdAt: string;
  hasResponded: boolean;
  id: string;
  openedAt?: string | null;
  questions: Array<PollQuestion>;
  quizMode: boolean;
  responseCount: number;
  resultsVisibility: ResultsVisibility;
  status: PollStatus;
  title: string;
  version: number;
}


export interface PreJoinAccess {
  accessPolicy: AccessPolicy;
  alreadyAuthorized: boolean;
  authenticationRequired: boolean;
  /** Decided by an external authority (e.g. a healthcare appointment). */
  externalAuthorization: boolean;
  invitationRequired: boolean;
  passcodeRequired: boolean;
  waitingRoomExpected: boolean;
}


export interface PreJoinConsents {
  /** Already granted by the caller's participant (rejoin). */
  granted: Array<ConsentKind>;
  /** Must be granted (join request `consents`) before admission. */
  required: Array<ConsentKind>;
  version: string;
}

/** Client-side device contract. The backend never needs device ids for authorization. */
export interface PreJoinDevices {
  /** Background blur / replacement run client-side only. */
  backgroundEffects: string;
  cameraPreview: boolean;
  cameraSelection: boolean;
  /** Device ids, labels and fingerprints are never sent or stored. */
  deviceIdentifiersCollected: boolean;
  microphoneSelection: boolean;
  microphoneTest: boolean;
  /** Speaker selection depends on the platform (setSinkId support). */
  speakerSelection: string;
}


export interface PreJoinDisplayName {
  editable: boolean;
  /** Always false: a display name never selects identity, role or participant. */
  identityBearing: boolean;
  /** Suggested name (profile name for registered users). */
  suggested?: string | null;
}


export interface PreJoinMedia {
  audioLocked: boolean;
  permissions: JoinPermissions;
  role: Role;
  screenShareEligible: boolean;
  screenSharePolicy: ScreenSharePolicy;
  videoLocked: boolean;
}


export interface PreJoinNotices {
  aiNotice: boolean;
  recordingActive: boolean;
  recordingPolicy: MediaCapturePolicy;
  transcriptionActive: boolean;
  transcriptionPolicy: MediaCapturePolicy;
}


export interface PreJoinOrganization {
  id: string;
  name: string;
}


export interface PreJoinOrganizer {
  displayName: string;
}


export interface PreJoinResponse {
  access: PreJoinAccess;
  conferenceId: string;
  conferenceStatus: ConferenceStatus;
  consents: PreJoinConsents;
  devices: PreJoinDevices;
  displayName: PreJoinDisplayName;
  /** Media join is possible right now (LIVE). */
  joinable: boolean;
  media: PreJoinMedia;
  meetingSpaceId: string;
  meetingType: MeetingType;
  notices: PreJoinNotices;
  organization?: PreJoinOrganization | null;
  organizer?: PreJoinOrganizer | null;
  scheduledStartAt?: string | null;
  /** Hidden for sensitive (healthcare / high-security) meetings until authorized. */
  title?: string | null;
}


export type PrincipalType = "USER" | "EMAIL";


export type PrivateChatMode = "EVERYONE" | "HOSTS_ONLY" | "DISABLED";

/** Where processing may run. Ordered from most to least restrictive. */
export type ProcessingLocation = "DISABLED" | "SELF_HOSTED_ONLY" | "EXTERNAL_ALLOWED";

/** The complete effective profile policy of a space / conference. */
export interface ProfilePolicy {
  access: AccessProfile;
  audit: AuditProfile;
  dataSharing: DataSharingProfile;
  exports: ExportProfile;
  intelligence: IntelligenceProfile;
  recording: RecordingProfile;
  registration: RegistrationProfile;
  roles: RoleMappingProfile;
  stage: StageProfile;
  transcription: TranscriptionProfile;
}


export interface ProfilePolicyResponse {
  effective: ResolvedProfile;
  meetingSpaceId: string;
  /** The stored sparse space override. */
  overrides: Record<string, unknown>;
  profilePolicyVersion: number;
}


export interface ProfilePolicyUpdateRequest {
  /** Sparse override (validated against the profile schema; unknown keys rejected). */
  changes: Record<string, unknown>;
  /** Optimistic concurrency (`profilePolicyVersion` from the last read). */
  profilePolicyVersion: number;
}


export type ProviderDispatch = "DISPATCHED" | "PENDING_RETRY" | "NOT_REQUIRED";

/** Provider enforcement state for operations whose media effect is Part E. */
export type ProviderEnforcement = "PENDING_PROVIDER_CONTROL" | "ENFORCED" | "NOT_REQUIRED";

/** API response wrapper for public meeting info */
export interface PublicMeetingInfoApiResponse {
  data: PublicMeetingInfoResponse;
  success: boolean;
}

/**
 * Public meeting info response (no authentication required)
 * Used by the hosted UI to display meeting details before joining
 */
export interface PublicMeetingInfoResponse {
  /** End time in UTC. Format: "2026-01-25T15:00:00Z" */
  end_time?: string | null;
  is_private: boolean;
  /** Join URL for the hosted UI */
  join_url: string;
  meeting_identifier: string;
  /** Start time in UTC. Format: "2026-01-25T14:00:00Z" */
  start_time: string;
  status: string;
  title: string;
}


export interface QualityIngestResponse {
  accepted: number;
  duplicateBatch: boolean;
  participantSessionId: string;
  qualityState: string;
  /** Samples the telemetry sink could not take (meeting unaffected). */
  sinkDropped: number;
}


export interface QualitySampleBatch {
  /** Client batch id (idempotency for retries; 1–64 chars). */
  batchId?: string | null;
  /**
   * Optional: one of the caller's own active sessions (resolved server-side
   * otherwise). Another participant's session is rejected.
   */
  participantSessionId?: string | null;
  providerNode?: string | null;
  /** Provider edge the client connected to (sanitized; operations-only). */
  providerRegion?: string | null;
  samples: Array<QualitySampleInput>;
}


export interface QualitySampleInput {
  bitrateKbps?: number | null;
  failure?: MediaFailure | null;
  fps?: number | null;
  height?: number | null;
  jitterMs?: number | null;
  networkType?: NetworkType | null;
  packetLossRatio?: number | null;
  publishHealth?: MediaHealth | null;
  rttMs?: number | null;
  subscribeHealth?: MediaHealth | null;
  timestamp: string;
  width?: number | null;
}


export interface QuestionActionResponse {
  changed: boolean;
  question: QuestionView;
}


export interface QuestionListResponse {
  questions: Array<QuestionView>;
}


export interface QuestionResult {
  answered: number;
  /** Ranking: option id → average position (1 = first). */
  averagePositions?: Record<string, number> | null;
  /** Quiz: correct answer, once the results state allows it. */
  correct?: Array<string> | null;
  /** Choice: option id → votes. */
  optionCounts?: Record<string, number> | null;
  questionId: string;
  ratingAverage?: number | null;
  /** Rating: value → count, and the average. */
  ratingDistribution?: Record<string, number> | null;
  /** Text answers (managers only; never attributed for anonymous polls). */
  texts?: Array<TextAnswer> | null;
  type: PollQuestionType;
}


export type QuestionStatus = "OPEN" | "ANSWERED" | "DISMISSED" | "ARCHIVED";


export interface QuestionView {
  anonymous: boolean;
  answer?: string | null;
  answeredAt?: string | null;
  body: string;
  conferenceId: string;
  createdAt: string;
  displayName?: string | null;
  id: string;
  isMine: boolean;
  /** Absent for anonymous questions. */
  participantId?: string | null;
  pinned: boolean;
  status: QuestionStatus;
  upvoteCount: number;
  version: number;
  votedByMe: boolean;
}


export interface RaisedHand {
  displayName: string;
  participantId: string;
  raisedAt: string;
}


export interface RaisedHandsResponse {
  hands: Array<RaisedHand>;
}


export interface ReactionRequest {
  reaction: string;
}


export interface ReactionResponse {
  /** Transient: false when the realtime fanout was unavailable (nothing is stored). */
  delivered: boolean;
  reaction: string;
}

/** Stable provider-neutral realtime envelope. */
export interface RealtimeEnvelope {
  conferenceId?: string | null;
  correlationId?: string | null;
  eventId: string;
  eventType: string;
  meetingSpaceId?: string | null;
  occurredAt: string;
  payload: Record<string, unknown>;
  schemaVersion: number;
  /** Gap-free per-conference sequence (commit order) — the replay cursor. */
  sequence?: number | null;
}


export interface RealtimeTicketResponse {
  /** Current conference event sequence (start cursor for a fresh subscriber). */
  cursor: number;
  expiresAt: string;
  /** `FULL` (admitted / members) or `SELF_ONLY` (waiting participants). */
  scope: string;
  /** One-shot, short-lived ticket for `GET …/realtime?ticket=` (browser WebSockets). */
  ticket: string;
  /** WebSocket path relative to the API origin. */
  websocketPath: string;
}


export interface RecordConsentRequest {
  consentVersion: string;
  decisions: Array<ConsentDecisionInput>;
}


export type RecordOrigin = "V2" | "LEGACY";


export interface RecordingListResponse {
  recordings: Array<ArtifactView>;
}


export interface RecordingProfile {
  allowed: boolean;
  consentMode: ConsentMode;
  /** Composite layout (`grid` / `speaker`). */
  layout: string;
  manualDeleteAllowed: boolean;
  /** Days a READY recording is kept before the retention worker deletes it. */
  retentionDays: number;
  withdrawalMode: WithdrawalMode;
}


export interface RecordingResponse {
  /** False for an idempotent repeat. */
  created: boolean;
  providerDispatch: ProviderDispatch;
  recording: ArtifactView;
}


export type RecurrenceExceptionAction = "CANCEL" | "RESCHEDULE";


export type RecurrenceExceptionKind = "CANCELLED" | "RESCHEDULED";


export interface RecurrenceExceptionRequest {
  action: RecurrenceExceptionAction;
  /** The occurrence's original start (as generated by the series), any distance ahead. */
  originalStartAt: string;
  /** RESCHEDULE only; defaults to the series duration. */
  scheduledEndAt?: string | null;
  /** RESCHEDULE only. */
  scheduledStartAt?: string | null;
}


export interface RecurrenceExceptionResponse {
  /** Set when the occurrence was already materialized and has been updated. */
  appliedToConferenceId?: string | null;
  id: string;
  kind: RecurrenceExceptionKind;
  newEndAt?: string | null;
  newStartAt?: string | null;
  /** Original wall-clock start in the series timezone (exception key). */
  originalStartLocal: string;
  recurrenceRuleId: string;
  timezone: string;
  version: number;
}


export interface RecurrenceResponse {
  durationMinutes: number;
  id: string;
  materializedUntil?: string | null;
  meetingSpaceId: string;
  rrule: string;
  startLocal: string;
  status: RecurrenceStatus;
  timezone: string;
  version: number;
}


export type RecurrenceStatus = "ACTIVE" | "ENDED";


export interface RefreshTokenRequest {
  refresh_token: string;
}


export interface RefreshTokenResponse {
  access_token: string;
  refresh_token: string;
}


export interface RegisterRequest {
  confirm_password: string;
  email: string;
  password: string;
  username: string;
}


export interface RegisterResponse {
  email: string;
  role: string;
  /**
   * `pending_verification` (spelled `pendingverification` until
   * `ACCOUNT_STATUS_FORMAT=canonical`), `active`, `inactive` or `suspended`.
   */
  status: string;
  user_id: string;
}


export interface RegistrationCreated {
  /**
   * Opaque admission token (shown once; only its hash is stored). Absent for an
   * idempotent repeat.
   */
  admissionToken?: string | null;
  conferenceId?: string | null;
  created: boolean;
  expiresAt: string;
  meetingSpaceId: string;
  registrationId: string;
}


export interface RegistrationListResponse {
  registrations: Array<RegistrationView>;
}


export interface RegistrationProfile {
  customFields: Array<CustomRegistrationField>;
  email: FieldRequirement;
  enabled: boolean;
  organization: FieldRequirement;
  tokenMaxUses: number;
  tokenTtlHours: number;
}


export interface RegistrationRequest {
  /** Register for one occurrence (optional; otherwise the whole meeting series). */
  conferenceId?: string | null;
  /** Only keys approved by the profile policy are accepted (string values). */
  customFields?: Record<string, unknown> | null;
  displayName: string;
  email?: string | null;
  idempotencyKey?: string | null;
  organization?: string | null;
}


export interface RegistrationView {
  cancelledAt?: string | null;
  conferenceId?: string | null;
  createdAt: string;
  customFields: Record<string, unknown>;
  displayName: string;
  email?: string | null;
  id: string;
  meetingSpaceId: string;
  organization?: string | null;
  status: string;
}


export interface RemoveParticipantRequest {
  reason?: string | null;
}


export interface RescheduleConferenceRequest {
  /**
   * New end. Omitted: the occurrence keeps its duration (a start-only occurrence stays
   * start-only). At most 24 hours after the start.
   */
  scheduledEndAt?: string | null;
  /** New start; more than 5 minutes in the past is rejected. */
  scheduledStartAt: string;
  /** Optional optimistic-concurrency guard: the conference `version` last read. */
  version?: number | null;
}


export interface ResendVerificationRequest {
  email: string;
}


export interface ResetPasswordRequest {
  confirm_password: string;
  email: string;
  new_password: string;
  token: string;
}


export interface ResolveInvitationResponse {
  conferenceId?: string | null;
  expiresAt: string;
  meetingSpace: MeetingSpaceResponse;
  remainingUses?: number | null;
  requestedRole: Role;
}


export interface ResolveResponse {
  conference?: ConferenceResponse | null;
  /** How the identifier matched: V2_ID, V2_CODE, LEGACY_ID or LEGACY_CODE. */
  matchedBy: string;
  meetingSpace: MeetingSpaceResponse;
}

/** Effective policy plus provenance. */
export interface ResolvedProfile {
  /** Override fields that a ceiling clamped back (visible to administrators). */
  clampedFields: Array<string>;
  meetingType: MeetingType;
  /** A stored layer failed validation → restrictive fallback is in force. */
  misconfigured: boolean;
  policy: ProfilePolicy;
  profile: MeetingProfile;
  securityProfile: SecurityProfile;
}


export type ResourceView = "FULL" | "SUMMARY";


export type ResultsVisibility = "HOST_ONLY" | "AFTER_VOTE" | "AFTER_CLOSE";

/** Response for API key revocation */
export interface RevokeApiKeyResponse {
  message: string;
  success: boolean;
}


export type Role = "OWNER" | "HOST" | "CO_HOST" | "MODERATOR" | "PRESENTER" | "PARTICIPANT" | "VIEWER";


export interface RoleChangeRequest {
  role: Role;
  /** Optional optimistic version of the participant. */
  version?: number | null;
}


export interface RoleMappingProfile {
  /** External role token (e.g. `DOCTOR`) → generic meeting role. Never OWNER. */
  externalRoleMapping: Record<string, Role>;
}

/** Controlled per-space / organization overrides of POLICY cells only. */
export interface RolePolicy {
  /** role → permission key → allowed. Only POLICY cells may be overridden. */
  overrides?: Record<string, Record<string, boolean>>;
}


export type RoleSource = "OWNER" | "MEMBERSHIP" | "INVITATION" | "DEFAULT" | "ASSIGNED" | "LEGACY";


export interface RoomAssignmentsPatch {
  add?: Array<string>;
  remove?: Array<string>;
}


export interface RtcCredentialResponse {
  /** Short-lived provider credential. */
  credential: string;
  expiresAt: string;
  provider: string;
  url: string;
}


export type RtcStatus = "ISSUED" | "NOT_ISSUED_WAITING" | "PROVIDER_CONTROL_DISABLED";

/** Start-window / late-start / auto-end policy (minutes). */
export interface SchedulePolicy {
  autoEnd?: boolean;
  autoEndGraceMinutes?: number;
  earlyStartMinutes?: number;
  lateStartGraceMinutes?: number;
}

/** Who may publish a screen share (on top of the `screenshare.publish` role grant). */
export type ScreenSharePolicy = "HOST_ONLY" | "PRESENTERS_ONLY" | "ALL_INTERACTIVE";

/** Response for screen share action */
export interface ScreenShareResponse {
  is_screen_sharing: boolean;
  message: string;
  participant_id: string;
  success: boolean;
}


export interface SecurityEventPage {
  events: Array<SecurityEventRecord>;
  nextCursor?: number | null;
}

/** One account security audit event (administrator view; never secrets or contact data). */
export interface SecurityEventRecord {
  /** USER, ADMIN, SYSTEM or ANONYMOUS. */
  actorKind: string;
  actorUserId?: string | null;
  detail: Record<string, unknown>;
  eventId: string;
  eventType: string;
  occurredAt: string;
  /** SUCCESS, FAILURE or DENIED. */
  outcome: string;
  requestId?: string | null;
  /** Cursor value: pass it as `before` to page further back. */
  seq: number;
  sessionId?: string | null;
  userId?: string | null;
}


export type SecurityProfile = "STANDARD" | "BUSINESS" | "ENTERPRISE" | "HEALTHCARE" | "HIGH_SECURITY";


export interface SegmentView {
  confidence?: number | null;
  endMs: number;
  language: string;
  sequence: number;
  speakerParticipantId?: string | null;
  startMs: number;
  text: string;
}

/** Response for sending a chat message */
export interface SendChatMessageApiResponse {
  data: ChatMessageResponse;
  success: boolean;
}

/** Request to send a chat message */
export interface SendChatMessageRequest {
  message: string;
  /** The participant sending the message */
  participant_id: string;
}


export interface SendMessageRequest {
  /** Host-only announcement to the main room (`chat.moderate`). */
  announcement?: boolean;
  body: string;
  /** Breakout chat of this room (must be the sender's current breakout). */
  breakoutId?: string | null;
  /** Private message recipient (conference-scoped participant id). */
  recipientParticipantId?: string | null;
}

/** One account session as shown to its owner. */
export interface SessionRecord {
  /** PASSWORD, GOOGLE or LEGACY_REFRESH (a pre-session sign-in carried forward). */
  authMethod: string;
  authenticatedAt: string;
  /** Shortened User-Agent of the client that signed in. */
  clientLabel?: string | null;
  createdAt: string;
  /** True for the session the request was made with. */
  current: boolean;
  endReason?: string | null;
  endedAt?: string | null;
  /** The session ends here at the latest, whatever the refresh activity. */
  expiresAt: string;
  lastRefreshedAt?: string | null;
  sessionId: string;
  /** ACTIVE, REVOKED or EXPIRED. */
  status: string;
}


export type SessionState = "PENDING" | "CONNECTED" | "DISCONNECTED" | "REPLACED" | "TERMINATED" | "EXPIRED" | "HISTORICAL";


export interface SetRecurrenceRequest {
  durationMinutes: number;
  /** RFC 5545 RRULE body, e.g. `FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10`. */
  rrule: string;
  /** First occurrence, wall-clock time in the meeting space timezone. */
  startLocal: string;
}


export interface ShadowComparisonResponse {
  consistent: boolean;
  differences: Array<string>;
  legacyMeetingId: string;
  meetingSpaceId?: string | null;
}


export interface SkippedParticipant {
  admissionState: AdmissionState;
  participantId: string;
}


export type SpaceStatus = "ACTIVE" | "ARCHIVED" | "DELETED";


export interface SpotlightListResponse {
  changed: boolean;
  spotlights: Array<SpotlightView>;
}


export interface SpotlightRequest {
  participantId: string;
}


export interface SpotlightView {
  displayName: string;
  participantId: string;
  spotlightedAt: string;
}

/** Large-audience architecture: a small interactive stage, a subscribe-only audience. */
export interface StageProfile {
  backstageEnabled: boolean;
  /** PRESENTER-role participants. 0 = not limited. */
  maxPresenters: number;
  /** Participants that may publish (every role above VIEWER). 0 = not limited. */
  maxStageParticipants: number;
  /** Operational viewer capacity (informational; enforced by space capacity). */
  viewerCapacity: number;
}

/**
 * `POST /recordings` body. The client never selects provider ids, storage buckets
 * or keys.
 */
export interface StartRecordingRequest {
  /** Client idempotency key (repeats return the same recording). */
  idempotencyKey?: string | null;
  /** `GRID` (default from policy) or `SPEAKER`. */
  layout?: string | null;
}


export interface StartTranscriptionRequest {
  /** BCP-47-like language tag (e.g. `en`, `en-GB`). */
  language?: string | null;
  mode?: TranscriptionModeDto | null;
  /** POST_MEETING: the READY recording to transcribe. */
  sourceRecordingId?: string | null;
}


export interface StopScreenShareResponse {
  participantId: string;
  stoppedSessions: number;
}


export interface SubmitPollResponseRequest {
  answers: Array<PollAnswerInput>;
  /**
   * Client-chosen retry key: a retry with the same key is acknowledged
   * idempotently instead of failing as a duplicate ballot.
   */
  idempotencyKey?: string | null;
}


export interface SubmitQuestionRequest {
  anonymous?: boolean;
  body: string;
}


export type TerminationReason = "HOST_ENDED" | "SCHEDULED_END" | "ADMIN" | "FAILURE" | "EMPTY_TIMEOUT" | "CANCELLED" | "LEGACY";


export interface TextAnswer {
  /** Only for non-anonymous polls. */
  participantId?: string | null;
  text: string;
}

/** Media track kinds the business layer reasons about. */
export type TrackKind = "AUDIO" | "VIDEO" | "SCREEN_SHARE";


export interface TranscriptView {
  artifact: ArtifactView;
  job?: TranscriptionJobView | null;
  /** Cursor for the next page (`after` = last sequence). */
  nextAfter?: number | null;
  segments: Array<SegmentView>;
}


export interface TranscriptionJobListResponse {
  jobs: Array<TranscriptionJobView>;
}


export interface TranscriptionJobView {
  artifactId: string;
  conferenceId: string;
  endedAt?: string | null;
  failureCode?: string | null;
  id: string;
  language: string;
  mode: string;
  segmentCount: number;
  startedAt?: string | null;
  status: string;
}


export type TranscriptionModeDto = "LIVE" | "POST_MEETING";


export interface TranscriptionProfile {
  allowed: boolean;
  consentMode: ConsentMode;
  location: ProcessingLocation;
  retentionDays: number;
  withdrawalMode: WithdrawalMode;
}


export interface TransitionRequest {
  /** Second operator approving a cutover or retirement (never the requester). */
  approvedBy?: string | null;
  /** Optimistic concurrency: the version the operator looked at. */
  expectedVersion: number;
  /** Why (audited), 3–500 characters. */
  reason: string;
  targetState: ChatMigrationState;
}


export interface TransitionView {
  actor: string;
  approvedBy?: string | null;
  createdAt: string;
  fromState: string;
  reason: string;
  toState: string;
  version: number;
}


export interface TranslateRequest {
  targetLanguage: string;
}


export interface TranslationResponse {
  artifactId: string;
  targetLanguage: string;
  translatedSegments: number;
}

/** Fields to change. Omitted fields stay as they are; `null` clears an optional field. */
export interface UpdateAccountProfileRequest {
  /** `https://` avatar image reference (at most 512 characters); `null` clears it. */
  avatarUrl?: string | null;
  /** 1-64 characters shown to other people instead of the username; `null` clears it. */
  displayName?: string | null;
  /** BCP 47 language tag; `null` clears it. */
  locale?: string | null;
  /**
   * IANA time zone used to present times to you (for example in notification emails);
   * `null` clears it.
   */
  timezone?: string | null;
  /** 3-50 letters, digits or underscores (surrounding spaces are removed). Unique. */
  username?: string | null;
}

/** Request to update audio state */
export interface UpdateAudioRequest {
  is_muted: boolean;
}

/** `PATCH /api/v2/organizations/{id}/members/{userId}`. */
export interface UpdateBusinessMembershipRequest {
  /** The membership `version` the change is based on. */
  expectedVersion: number;
  /** OWNER, ADMIN, MANAGER, MEMBER or GUEST. */
  role?: string | null;
  /** ACTIVE, SUSPENDED or REMOVED. */
  status?: string | null;
}

/** `PATCH /api/v2/organizations/{id}`. */
export interface UpdateBusinessOrganizationRequest {
  /** The organization `version` the change is based on (optimistic concurrency). */
  expectedVersion: number;
  name?: string | null;
  profile?: OrganizationProfile | null;
  slug?: string | null;
}

/** Request for PATCH /api/v1/users/me */
export interface UpdateMeRequest {
  /** 3-50 letters, digits or underscores (validated after trimming, as at registration). */
  username?: string | null;
}

/** Request to update an existing meeting */
export interface UpdateMeetingRequest {
  /** End time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS" */
  end_time?: string | null;
  is_private?: boolean | null;
  metadata?: Record<string, unknown> | null;
  /** Start time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS" */
  start_time?: string | null;
  /** IANA timezone of the user (e.g., "Africa/Lagos"). Required when updating times. */
  timezone?: string | null;
  title?: string | null;
}


export interface UpdateMeetingSpaceRequest {
  accessConfig?: AccessConfig | null;
  accessPolicy?: AccessPolicy | null;
  clearPasscode?: boolean;
  collaborationPolicy?: CollaborationPolicyPatch | null;
  defaultRole?: Role | null;
  description?: string | null;
  interactiveCapacity?: number | null;
  meetingType?: MeetingType | null;
  passcode?: string | null;
  recordingPolicy?: MediaCapturePolicy | null;
  retentionPolicyRef?: string | null;
  rolePolicy?: RolePolicy | null;
  schedulePolicy?: SchedulePolicy | null;
  securityProfile?: SecurityProfile | null;
  timezone?: string | null;
  title?: string | null;
  transcriptionPolicy?: MediaCapturePolicy | null;
  /** Optimistic concurrency version the client last read. */
  version: number;
  viewerCapacity?: number | null;
  waitingRoomPolicy?: WaitingRoomPolicy | null;
}


export interface UpdateMemberRequest {
  role: Role;
  version: number;
}


export interface UpdateOrganizationRequest {
  status: OrganizationStatus;
}

/** Request to update video state */
export interface UpdateVideoRequest {
  is_video_on: boolean;
}


export interface UpdateWebhookRequest {
  endpoint_url?: string | null;
  /** Status: active or inactive */
  status?: string | null;
}


export interface UpsertOrganizationMemberRequest {
  role: OrganizationRole;
  status: OrganizationMemberStatus;
}


export interface UserInfo {
  id: string;
  role: string;
  /**
   * `pending_verification` (spelled `pendingverification` until
   * `ACCOUNT_STATUS_FORMAT=canonical`), `active`, `inactive` or `suspended`.
   */
  status: string;
  username: string;
}

/** Response for GET /api/v1/users/me */
export interface UserMeResponse {
  created_at: string;
  email: string;
  id: string;
  role: string;
  /**
   * `pending_verification` (spelled `pendingverification` until
   * `ACCOUNT_STATUS_FORMAT=canonical`), `active`, `inactive` or `suspended`.
   */
  status: string;
  username: string;
}


export interface VerifyEmailRequest {
  email: string;
  token: string;
}


export interface VerifyResetTokenRequest {
  email: string;
  token: string;
}


export interface VerifyResetTokenResponse {
  message: string;
  valid: boolean;
}


export interface VersionedRequest {
  version?: number | null;
}

/** Response for listing waiting participants */
export interface WaitingListResponse {
  data: Array<WaitingParticipantResponse>;
  success: boolean;
}

/** Waiting participant response */
export interface WaitingParticipantResponse {
  display_name: string;
  join_time: string;
  participant_id: string;
  user_id?: string | null;
}


export type WaitingRoomPolicy = "OFF" | "EVERYONE" | "GUESTS_ONLY" | "UNAUTHENTICATED_ONLY" | "NON_MEMBERS_ONLY" | "EXTERNAL_ONLY" | "CUSTOM";


export interface WaitingRoomResponse {
  conferenceId: string;
  participants: Array<ParticipantResponse>;
}


export interface WebhookApiResponse {
  data: WebhookResponse;
  /**
   * Deprecation notice: nothing registered here is delivered; use Developer Platform
   * webhooks.
   */
  notice: string;
  success: boolean;
}


export interface WebhookListResponse {
  data: Array<WebhookResponse>;
  /**
   * Deprecation notice: nothing registered here is delivered; use Developer Platform
   * webhooks.
   */
  notice: string;
  success: boolean;
}


export interface WebhookResponse {
  created_at: string;
  /**
   * Always `NOT_DELIVERED`: legacy meeting webhooks are never delivered (deprecated;
   * use Developer Platform webhooks).
   */
  delivery: string;
  endpoint_url: string;
  event_type: string;
  id: string;
  status: string;
  updated_at: string;
  user_id: string;
}


export type WithdrawalEffect = "RECORDED" | "PROCESSING_STOPPED" | "PARTICIPANT_EXITED";

/** What a consent withdrawal does (policy-driven, never one universal rule). */
export type WithdrawalMode = "STOP_FUTURE_PROCESSING" | "REQUIRE_PARTICIPANT_EXIT" | "STOP_PROCESSING_MODE" | "NO_RETROACTIVE_DELETION";


export interface WithdrawalResponse {
  consentType: ConsentKind;
  effect: WithdrawalEffect;
  /** Previously retained artefacts are not deleted by a withdrawal. */
  retroactiveDeletion: boolean;
  withdrawalMode: WithdrawalMode;
}

/** One workspace context of the caller. */
export interface Workspace {
  capabilities: WorkspaceCapabilitySummary;
  /** `Personal`, or the organization name. */
  displayName: string;
  /** `personal`, or the organization id. */
  id: string;
  kind: WorkspaceKind;
  /** ORGANIZATION only. */
  organizationId?: string | null;
  /** ACTIVE or SUSPENDED (a suspended organization grants nothing). ORGANIZATION only. */
  organizationStatus?: string | null;
  /**
   * NOT_REQUIRED, PENDING, IN_PROGRESS, READY or FAILED, from the Organizations Service;
   * null for PERSONAL and when the legacy directory answers (no provisioning there).
   */
  provisioningState?: string | null;
  /**
   * The caller's role (OWNER, ADMIN, MANAGER, MEMBER or GUEST; the legacy directory knows
   * OWNER, ADMIN and MEMBER). ORGANIZATION only.
   */
  role?: string | null;
  /** ORGANIZATION only. */
  slug?: string | null;
}

/**
 * Which store answered: the meeting-service organization directory
 * (`ORGANIZATIONS_AUTHORITY_MODE` legacy / shadow) or the Organizations Service
 * (authoritative).
 */
export type WorkspaceAuthority = "LEGACY_DIRECTORY" | "ORGANIZATIONS_SERVICE";

/** `GET /api/v2/me/workspaces/{workspaceId}/capabilities`. */
export interface WorkspaceCapabilities {
  authority: WorkspaceAuthority;
  /** Every capability of the vocabulary with its decision. */
  capabilities: Array<WorkspaceCapability>;
  /** When the answer was computed (it is never cached server-side). */
  evaluatedAt: string;
  features: WorkspaceFeatures;
  kind: WorkspaceKind;
  organizationId?: string | null;
  organizationStatus?: string | null;
  /**
   * PERSONAL only: the Communications Hub read that owns personal communications
   * capabilities (`/api/v2/communications/me/capabilities`).
   */
  personalCommunicationsCapabilities?: string | null;
  /** Organization policy revision the answer was computed with (Organizations Service). */
  policyRevision?: number | null;
  provisioningState?: string | null;
  role?: string | null;
  /** `personal`, or the organization id. */
  workspaceId: string;
}

/** One capability decision. */
export interface WorkspaceCapability {
  /** Capability name, e.g. `meetings.organization.create`. */
  capability: string;
  granted: boolean;
  reason: WorkspaceCapabilityReason;
}

/** Why a capability is (not) granted in a workspace. */
export type WorkspaceCapabilityReason = "ALLOWED" | "CAPABILITY_NOT_GRANTED" | "POLICY_RESTRICTED" | "ORGANIZATION_SUSPENDED" | "FEATURE_DISABLED";

/** A small summary of what the caller can do in a workspace. */
export interface WorkspaceCapabilitySummary {
  /** Organization administration: profile, member management or policies. */
  canAdminister: boolean;
  /** Meetings can be created in this context. */
  canCreateMeetings: boolean;
  /** Members can be invited (`organization.members.invite`). */
  canInviteMembers: boolean;
  /** Every capability granted now (default deny). */
  granted: Array<string>;
}

/**
 * Features the workspace context does not disable. Owner services still apply their
 * own policy (stricter wins) and re-verify every request.
 */
export interface WorkspaceFeatures {
  calling: boolean;
  meetings: boolean;
  messaging: boolean;
}

/** Kind of workspace context. */
export type WorkspaceKind = "PERSONAL" | "ORGANIZATION";

/** `GET /api/v2/me/workspaces`. */
export interface WorkspacePage {
  authority: WorkspaceAuthority;
  /** PERSONAL first, then organizations ordered by id. */
  items: Array<Workspace>;
  /** Pass back as `cursor` for the next page; null on the last page. */
  nextCursor?: string | null;
}

/** Every served operation: `METHOD /path` → operationId. */
export const CONTRACT_OPERATIONS = {
  "DELETE /api/v1/api-keys/{id}": "revoke_api_key",
  "DELETE /api/v1/meetings/{id}": "delete_meeting",
  "DELETE /api/v1/webhooks/{id}": "delete_webhook",
  "DELETE /api/v2/conferences/{id}/artifacts/{artifactId}": "v2_delete_artifact",
  "DELETE /api/v2/conferences/{id}/backstage": "v2_leave_backstage",
  "DELETE /api/v2/conferences/{id}/hand-raise": "v2_lower_hand",
  "DELETE /api/v2/conferences/{id}/messages/{messageId}": "v2_delete_message",
  "DELETE /api/v2/conferences/{id}/participants/{participantId}/audio-lock": "v2_audio_unlock",
  "DELETE /api/v2/conferences/{id}/participants/{participantId}/chat-restriction": "v2_unrestrict_chat",
  "DELETE /api/v2/conferences/{id}/participants/{participantId}/hand-raise": "v2_lower_other_hand",
  "DELETE /api/v2/conferences/{id}/participants/{participantId}/screen-share-lock": "v2_screen_share_unlock",
  "DELETE /api/v2/conferences/{id}/participants/{participantId}/video-lock": "v2_video_unlock",
  "DELETE /api/v2/conferences/{id}/questions/{questionId}/pin": "v2_unpin_question",
  "DELETE /api/v2/conferences/{id}/questions/{questionId}/vote": "v2_unvote_question",
  "DELETE /api/v2/conferences/{id}/spotlights/{participantId}": "v2_remove_spotlight",
  "DELETE /api/v2/meeting-spaces/{id}/members/{memberId}": "v2_revoke_member",
  "DELETE /api/v2/meeting-spaces/{id}/recurrence": "v2_end_recurrence",
  "DELETE /api/v2/meeting-spaces/{id}/registrations/{registrationId}": "v2_cancel_registration",
  "DELETE /api/v2/messaging/relay/{path}": "v2_messaging_relay_delete",
  "DELETE /api/v2/organizations/{id}/calling/{path}": "v2_organization_calling_delete",
  "GET /": "get_app_info",
  "GET /api/v1/admin/logs": "list_logs",
  "GET /api/v1/admin/meetings": "admin_list_meetings",
  "GET /api/v1/admin/users": "list_users",
  "GET /api/v1/api-keys": "list_api_keys",
  "GET /api/v1/api-keys/{id}": "get_api_key",
  "GET /api/v1/auth/oauth/google": "oauth_google_init",
  "GET /api/v1/auth/oauth/google/callback": "oauth_google_callback",
  "GET /api/v1/meetings": "list_meetings",
  "GET /api/v1/meetings/public/{meeting_identifier}": "get_public_meeting_info",
  "GET /api/v1/meetings/{id}": "get_meeting",
  "GET /api/v1/meetings/{id}/chat": "get_messages",
  "GET /api/v1/meetings/{meeting_id}/participants": "list_participants",
  "GET /api/v1/meetings/{meeting_id}/waiting": "list_waiting_participants",
  "GET /api/v1/users/me": "get_current_user",
  "GET /api/v1/webhooks": "list_webhooks",
  "GET /api/v2/admin/audit-events": "v2_admin_list_account_audit_events",
  "GET /api/v2/admin/chat-migration": "v2_chat_migration_status",
  "GET /api/v2/admin/compat/legacy-meetings/{id}/shadow": "v2_shadow_compare",
  "GET /api/v2/admin/organizations": "v2_admin_list_organizations",
  "GET /api/v2/conferences/{id}": "v2_get_conference",
  "GET /api/v2/conferences/{id}/artifacts": "v2_list_artifacts",
  "GET /api/v2/conferences/{id}/artifacts/{artifactId}": "v2_get_artifact",
  "GET /api/v2/conferences/{id}/attendance": "v2_conference_attendance",
  "GET /api/v2/conferences/{id}/breakouts": "v2_list_breakouts",
  "GET /api/v2/conferences/{id}/calendar.ics": "v2_conference_calendar",
  "GET /api/v2/conferences/{id}/collaboration": "v2_collaboration_state",
  "GET /api/v2/conferences/{id}/consents/requirements": "v2_consent_requirements",
  "GET /api/v2/conferences/{id}/events": "v2_replay_events",
  "GET /api/v2/conferences/{id}/hand-raise": "v2_raised_hands",
  "GET /api/v2/conferences/{id}/messages": "v2_list_messages",
  "GET /api/v2/conferences/{id}/participants": "v2_list_participants",
  "GET /api/v2/conferences/{id}/participants/me": "v2_get_me",
  "GET /api/v2/conferences/{id}/polls": "v2_list_polls",
  "GET /api/v2/conferences/{id}/polls/{pollId}": "v2_get_poll",
  "GET /api/v2/conferences/{id}/polls/{pollId}/results": "v2_poll_results",
  "GET /api/v2/conferences/{id}/pre-join": "v2_pre_join",
  "GET /api/v2/conferences/{id}/quality": "v2_conference_quality",
  "GET /api/v2/conferences/{id}/questions": "v2_list_questions",
  "GET /api/v2/conferences/{id}/realtime": "v2_realtime_subscribe",
  "GET /api/v2/conferences/{id}/recordings": "v2_list_recordings",
  "GET /api/v2/conferences/{id}/recordings/{recordingId}": "v2_get_recording",
  "GET /api/v2/conferences/{id}/transcripts": "v2_list_transcripts",
  "GET /api/v2/conferences/{id}/transcripts/{artifactId}": "v2_get_transcript",
  "GET /api/v2/conferences/{id}/waiting-room": "v2_get_waiting_room",
  "GET /api/v2/me/conferences": "v2_my_conferences",
  "GET /api/v2/me/organizations": "v2_my_organizations",
  "GET /api/v2/me/profile": "v2_get_my_profile",
  "GET /api/v2/me/sessions": "v2_list_my_sessions",
  "GET /api/v2/me/workspaces": "v2_my_workspaces",
  "GET /api/v2/me/workspaces/{workspaceId}/capabilities": "v2_my_workspace_capabilities",
  "GET /api/v2/meeting-spaces": "v2_list_meeting_spaces",
  "GET /api/v2/meeting-spaces/{id}": "v2_get_meeting_space",
  "GET /api/v2/meeting-spaces/{id}/calendar.ics": "v2_meeting_space_calendar",
  "GET /api/v2/meeting-spaces/{id}/conferences": "v2_list_conferences",
  "GET /api/v2/meeting-spaces/{id}/invitations": "v2_list_invitations",
  "GET /api/v2/meeting-spaces/{id}/members": "v2_list_members",
  "GET /api/v2/meeting-spaces/{id}/profile-policy": "v2_get_profile_policy",
  "GET /api/v2/meeting-spaces/{id}/recurrence": "v2_get_recurrence",
  "GET /api/v2/meeting-spaces/{id}/recurrence/exceptions": "v2_list_recurrence_exceptions",
  "GET /api/v2/meeting-spaces/{id}/registrations": "v2_list_registrations",
  "GET /api/v2/meeting-spaces/{id}/registrations/{registrationId}": "v2_get_registration",
  "GET /api/v2/messaging/directory": "v2_messaging_directory",
  "GET /api/v2/messaging/me": "v2_messaging_me",
  "GET /api/v2/messaging/relay/{path}": "v2_messaging_relay_get",
  "GET /api/v2/organizations/{id}": "v2_get_business_organization",
  "GET /api/v2/organizations/{id}/audit-events": "v2_list_organization_audit_events",
  "GET /api/v2/organizations/{id}/calling/{path}": "v2_organization_calling_get",
  "GET /api/v2/organizations/{id}/invitations": "v2_list_organization_invitations",
  "GET /api/v2/organizations/{id}/members": "v2_list_organization_members",
  "GET /api/v2/organizations/{id}/provisioning": "v2_get_organization_provisioning",
  "GET /api/v2/resolve/{identifier}": "v2_resolve_identifier",
  "GET /health": "health_check",
  "PATCH /api/v1/admin/users/{id}": "update_user",
  "PATCH /api/v1/participants/{participant_id}/audio": "update_audio",
  "PATCH /api/v1/participants/{participant_id}/video": "update_video",
  "PATCH /api/v1/users/me": "update_current_user",
  "PATCH /api/v1/webhooks/{id}": "update_webhook",
  "PATCH /api/v2/admin/organizations/{id}": "v2_update_organization",
  "PATCH /api/v2/conferences/{id}/breakouts/{breakoutId}/assignments": "v2_patch_breakout_assignments",
  "PATCH /api/v2/conferences/{id}/chat-policy": "v2_update_chat_policy",
  "PATCH /api/v2/conferences/{id}/collaboration-policy": "v2_update_collaboration_policy",
  "PATCH /api/v2/conferences/{id}/participants/{participantId}/role": "v2_change_role",
  "PATCH /api/v2/me/profile": "v2_update_my_profile",
  "PATCH /api/v2/meeting-spaces/{id}": "v2_update_meeting_space",
  "PATCH /api/v2/meeting-spaces/{id}/members/{memberId}": "v2_update_member",
  "PATCH /api/v2/meeting-spaces/{id}/profile-policy": "v2_update_profile_policy",
  "PATCH /api/v2/messaging/relay/{path}": "v2_messaging_relay_patch",
  "PATCH /api/v2/organizations/{id}": "v2_update_business_organization",
  "PATCH /api/v2/organizations/{id}/calling/{path}": "v2_organization_calling_patch",
  "PATCH /api/v2/organizations/{id}/members/{userId}": "v2_update_organization_membership",
  "POST /api/v1/api-keys": "create_api_key",
  "POST /api/v1/api/meetings": "api_create_meeting",
  "POST /api/v1/api/meetings/{id}/join": "api_join_meeting",
  "POST /api/v1/auth/change-password": "change_password",
  "POST /api/v1/auth/forgot-password": "forgot_password",
  "POST /api/v1/auth/login": "login",
  "POST /api/v1/auth/logout": "logout",
  "POST /api/v1/auth/oauth/google/exchange": "oauth_google_exchange",
  "POST /api/v1/auth/refresh": "refresh_token",
  "POST /api/v1/auth/register": "register",
  "POST /api/v1/auth/resend-verification": "resend_verification_email",
  "POST /api/v1/auth/reset-password": "reset_password",
  "POST /api/v1/auth/verify-email": "verify_email",
  "POST /api/v1/auth/verify-reset-token": "verify_reset_token",
  "POST /api/v1/meetings": "create_meeting",
  "POST /api/v1/meetings/instant": "create_instant_meeting",
  "POST /api/v1/meetings/join/{meeting_identifier}": "join_meeting_by_identifier",
  "POST /api/v1/meetings/{id}/chat": "send_message",
  "POST /api/v1/meetings/{id}/end": "end_meeting",
  "POST /api/v1/meetings/{id}/join": "join_meeting",
  "POST /api/v1/meetings/{meeting_id}/screen-share/start": "start_screen_share",
  "POST /api/v1/meetings/{meeting_id}/screen-share/stop": "stop_screen_share",
  "POST /api/v1/meetings/{meeting_id}/waiting/{participant_id}/admit": "admit_participant",
  "POST /api/v1/meetings/{meeting_id}/waiting/{participant_id}/deny": "deny_participant",
  "POST /api/v1/participants/{participant_id}/kick": "kick_participant",
  "POST /api/v1/users/me/deactivate": "deactivate_account",
  "POST /api/v1/webhooks": "create_webhook",
  "POST /api/v2/admin/chat-migration/parity-runs": "v2_chat_migration_parity",
  "POST /api/v2/admin/chat-migration/transitions": "v2_chat_migration_transition",
  "POST /api/v2/admin/compat/backfill": "v2_run_backfill",
  "POST /api/v2/admin/organizations": "v2_create_organization",
  "POST /api/v2/conferences/{id}/artifacts/reports": "v2_generate_report",
  "POST /api/v2/conferences/{id}/artifacts/{artifactId}/access": "v2_artifact_access",
  "POST /api/v2/conferences/{id}/backstage": "v2_enter_backstage",
  "POST /api/v2/conferences/{id}/breakouts": "v2_create_breakouts",
  "POST /api/v2/conferences/{id}/breakouts/announcement": "v2_breakout_announcement_all",
  "POST /api/v2/conferences/{id}/breakouts/assign": "v2_assign_breakouts",
  "POST /api/v2/conferences/{id}/breakouts/close-all": "v2_close_all_breakouts",
  "POST /api/v2/conferences/{id}/breakouts/leave": "v2_leave_breakout_visit",
  "POST /api/v2/conferences/{id}/breakouts/move": "v2_move_participant",
  "POST /api/v2/conferences/{id}/breakouts/{breakoutId}/announcement": "v2_breakout_announcement",
  "POST /api/v2/conferences/{id}/breakouts/{breakoutId}/close": "v2_close_breakout",
  "POST /api/v2/conferences/{id}/breakouts/{breakoutId}/enter": "v2_enter_breakout",
  "POST /api/v2/conferences/{id}/breakouts/{breakoutId}/open": "v2_open_breakout",
  "POST /api/v2/conferences/{id}/cancel": "v2_cancel_conference",
  "POST /api/v2/conferences/{id}/consents": "v2_record_consents",
  "POST /api/v2/conferences/{id}/consents/{consentType}/withdraw": "v2_withdraw_consent",
  "POST /api/v2/conferences/{id}/end": "v2_end_conference",
  "POST /api/v2/conferences/{id}/hand-raise": "v2_raise_hand",
  "POST /api/v2/conferences/{id}/intelligence/requests": "v2_request_intelligence",
  "POST /api/v2/conferences/{id}/join": "v2_join_conference",
  "POST /api/v2/conferences/{id}/leave": "v2_leave_conference",
  "POST /api/v2/conferences/{id}/lock": "v2_lock_conference",
  "POST /api/v2/conferences/{id}/media-credential": "v2_media_credential",
  "POST /api/v2/conferences/{id}/messages": "v2_send_message",
  "POST /api/v2/conferences/{id}/participants/{participantId}/audio-lock": "v2_audio_lock",
  "POST /api/v2/conferences/{id}/participants/{participantId}/ban": "v2_ban",
  "POST /api/v2/conferences/{id}/participants/{participantId}/chat-restriction": "v2_restrict_chat",
  "POST /api/v2/conferences/{id}/participants/{participantId}/mute": "v2_mute_participant",
  "POST /api/v2/conferences/{id}/participants/{participantId}/remove": "v2_remove",
  "POST /api/v2/conferences/{id}/participants/{participantId}/return-to-waiting": "v2_return_to_waiting",
  "POST /api/v2/conferences/{id}/participants/{participantId}/screen-share-lock": "v2_screen_share_lock",
  "POST /api/v2/conferences/{id}/participants/{participantId}/video-lock": "v2_video_lock",
  "POST /api/v2/conferences/{id}/polls": "v2_create_poll",
  "POST /api/v2/conferences/{id}/polls/{pollId}/archive": "v2_archive_poll",
  "POST /api/v2/conferences/{id}/polls/{pollId}/close": "v2_close_poll",
  "POST /api/v2/conferences/{id}/polls/{pollId}/open": "v2_open_poll",
  "POST /api/v2/conferences/{id}/polls/{pollId}/responses": "v2_submit_poll_response",
  "POST /api/v2/conferences/{id}/prepare": "v2_prepare_conference",
  "POST /api/v2/conferences/{id}/quality/samples": "v2_submit_quality_samples",
  "POST /api/v2/conferences/{id}/questions": "v2_submit_question",
  "POST /api/v2/conferences/{id}/questions/{questionId}/answer": "v2_answer_question",
  "POST /api/v2/conferences/{id}/questions/{questionId}/archive": "v2_archive_question",
  "POST /api/v2/conferences/{id}/questions/{questionId}/dismiss": "v2_dismiss_question",
  "POST /api/v2/conferences/{id}/questions/{questionId}/pin": "v2_pin_question",
  "POST /api/v2/conferences/{id}/questions/{questionId}/vote": "v2_vote_question",
  "POST /api/v2/conferences/{id}/reactions": "v2_send_reaction",
  "POST /api/v2/conferences/{id}/realtime/ticket": "v2_realtime_ticket",
  "POST /api/v2/conferences/{id}/recordings": "v2_start_recording",
  "POST /api/v2/conferences/{id}/recordings/{recordingId}/stop": "v2_stop_recording",
  "POST /api/v2/conferences/{id}/reschedule": "v2_reschedule_conference",
  "POST /api/v2/conferences/{id}/screen-share/{participantId}/stop": "v2_stop_screen_share",
  "POST /api/v2/conferences/{id}/spotlights": "v2_spotlight",
  "POST /api/v2/conferences/{id}/start": "v2_start_conference",
  "POST /api/v2/conferences/{id}/transcription": "v2_start_transcription",
  "POST /api/v2/conferences/{id}/transcription/{jobId}/stop": "v2_stop_transcription",
  "POST /api/v2/conferences/{id}/transcripts/{artifactId}/translations": "v2_translate_transcript",
  "POST /api/v2/conferences/{id}/unlock": "v2_unlock_conference",
  "POST /api/v2/conferences/{id}/waiting-room/admit": "v2_admit",
  "POST /api/v2/conferences/{id}/waiting-room/deny": "v2_deny",
  "POST /api/v2/instant-meetings": "v2_create_instant_meeting",
  "POST /api/v2/invitations/{token}/resolve": "v2_resolve_invitation",
  "POST /api/v2/me/sessions/revoke-all": "v2_revoke_all_my_sessions",
  "POST /api/v2/me/sessions/{sessionId}/revoke": "v2_revoke_my_session",
  "POST /api/v2/meeting-spaces": "v2_create_meeting_space",
  "POST /api/v2/meeting-spaces/{id}/archive": "v2_archive_meeting_space",
  "POST /api/v2/meeting-spaces/{id}/conferences": "v2_create_conference",
  "POST /api/v2/meeting-spaces/{id}/duplicate": "v2_duplicate_meeting_space",
  "POST /api/v2/meeting-spaces/{id}/invitations": "v2_create_invitation",
  "POST /api/v2/meeting-spaces/{id}/invitations/{invitationId}/revoke": "v2_revoke_invitation",
  "POST /api/v2/meeting-spaces/{id}/members": "v2_add_member",
  "POST /api/v2/meeting-spaces/{id}/owner-transfer": "v2_transfer_owner",
  "POST /api/v2/meeting-spaces/{id}/registrations": "v2_register",
  "POST /api/v2/messaging/client-sessions/revoke-all": "v2_messaging_revoke_all_sessions",
  "POST /api/v2/messaging/client-tokens": "v2_messaging_client_token",
  "POST /api/v2/messaging/devices": "v2_messaging_register_device",
  "POST /api/v2/messaging/devices/registration-challenges": "v2_messaging_registration_challenge",
  "POST /api/v2/messaging/devices/{deviceId}/key-packages": "v2_messaging_publish_key_packages",
  "POST /api/v2/messaging/devices/{deviceId}/revoke": "v2_messaging_revoke_device",
  "POST /api/v2/messaging/directory/resolve": "v2_messaging_directory_resolve",
  "POST /api/v2/messaging/principals/names": "v2_messaging_principal_names",
  "POST /api/v2/messaging/relay/{path}": "v2_messaging_relay_post",
  "POST /api/v2/messaging/session-tokens": "v2_messaging_session_token",
  "POST /api/v2/organization-invitations/accept": "v2_accept_organization_invitation",
  "POST /api/v2/organization-invitations/decline": "v2_decline_organization_invitation",
  "POST /api/v2/organizations": "v2_create_business_organization",
  "POST /api/v2/organizations/{id}/archive": "v2_archive_business_organization",
  "POST /api/v2/organizations/{id}/calling/{path}": "v2_organization_calling_post",
  "POST /api/v2/organizations/{id}/invitations": "v2_create_organization_invitation",
  "POST /api/v2/organizations/{id}/invitations/{invitationId}/revoke": "v2_revoke_organization_invitation",
  "POST /api/v2/organizations/{id}/members/me/leave": "v2_leave_organization",
  "POST /api/v2/organizations/{id}/ownership-transfer": "v2_transfer_organization_ownership",
  "POST /api/v2/organizations/{id}/reactivate": "v2_reactivate_business_organization",
  "POST /api/v2/organizations/{id}/suspend": "v2_suspend_business_organization",
  "PUT /api/v1/meetings/{id}": "update_meeting",
  "PUT /api/v2/meeting-spaces/{id}/recurrence": "v2_set_recurrence",
  "PUT /api/v2/meeting-spaces/{id}/recurrence/exceptions": "v2_set_recurrence_exception",
  "PUT /api/v2/messaging/relay/{path}": "v2_messaging_relay_put",
  "PUT /api/v2/organizations/{id}/calling/{path}": "v2_organization_calling_put",
  "PUT /api/v2/organizations/{id}/members/{userId}": "v2_upsert_organization_member",
} as const;

export type ContractOperation = keyof typeof CONTRACT_OPERATIONS;

/** Operations the contract marks `deprecated: true`. */
export const DEPRECATED_OPERATIONS: readonly ContractOperation[] = [
  "DELETE /api/v1/webhooks/{id}",
  "GET /api/v1/webhooks",
  "PATCH /api/v1/webhooks/{id}",
  "POST /api/v1/api/meetings",
  "POST /api/v1/meetings",
  "POST /api/v1/meetings/instant",
  "POST /api/v1/webhooks",
];
