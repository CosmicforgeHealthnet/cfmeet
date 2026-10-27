/**
 * CF Meet V2 API Types
 * Matches the backend Rust DTOs at services/meeting-service/src/dto/v2.rs
 * All responses use camelCase JSON serialization
 */
export type * from "../messaging";

// ---------------------------------------------------------------------------
// Common Enums
// ---------------------------------------------------------------------------

export type MeetingType = "STANDARD" | "HEALTHCARE" | "WEBINAR" | "TOWN_HALL" | "TRAINING";

export type SpaceStatus = "ACTIVE" | "ARCHIVED" | "DELETED";

export type AccessPolicy =
  | "PUBLIC"
  | "PUBLIC_PASSCODE"
  | "AUTHENTICATED"
  | "ORGANIZATION_ONLY"
  | "DOMAIN_RESTRICTED"
  | "INVITE_ONLY"
  | "CUSTOM"
  | "HEALTHCARE_APPOINTMENT";

export type WaitingRoomPolicy = "OFF" | "GUESTS_ONLY" | "EVERYONE" | "UNAUTHENTICATED_ONLY" | "NON_MEMBERS_ONLY" | "EXTERNAL_ONLY" | "CUSTOM";

export type SecurityProfile = "STANDARD" | "BUSINESS" | "ENTERPRISE" | "HEALTHCARE" | "HIGH_SECURITY";

export type Role = "OWNER" | "HOST" | "CO_HOST" | "MODERATOR" | "PRESENTER" | "PARTICIPANT" | "VIEWER";

export type MediaCapturePolicy = "DISABLED" | "HOST_OPTIONAL" | "AUTO_START" | "ORGANIZATION_CONTROLLED";

export type ConferenceStatus =
  | "CREATED"
  | "PREPARING"
  | "WAITING"
  | "LIVE"
  | "ENDING"
  | "ENDED"
  | "CANCELLED"
  | "FAILED";

export type ConferenceKind = "INSTANT" | "SCHEDULED" | "RECURRING" | "LEGACY";

export type TerminationReason =
  | "HOST_ENDED"
  | "SCHEDULED_END"
  | "ADMIN"
  | "FAILURE"
  | "EMPTY_TIMEOUT"
  | "LEGACY"
  | "CANCELLED";

export type IdentityType = "REGISTERED_USER" | "INVITED_EXTERNAL" | "GUEST" | "PHONE" | "SERVICE" | "BOT";

export type AdmissionState = "WAITING" | "ADMITTED" | "DENIED" | "REMOVED" | "BANNED";

export type PrincipalType = "USER" | "EMAIL";

export type MemberStatus = "ACTIVE" | "REVOKED";

export type InvitationStatus = "ACTIVE" | "REVOKED";

export type RecurrenceStatus = "ACTIVE" | "ENDED";

export type RecurrenceExceptionKind = "CANCELLED" | "RESCHEDULED";

export type BanScope = "CONFERENCE" | "SPACE";

export type ResourceView = "FULL" | "SUMMARY";

export type ProviderEnforcement = "PENDING_PROVIDER_CONTROL" | "ENFORCED" | "NOT_REQUIRED";

export type RtcStatus = "ISSUED" | "NOT_ISSUED_WAITING" | "PROVIDER_CONTROL_DISABLED";

export type OrganizationStatus = "ACTIVE" | "SUSPENDED";

export type OrganizationRole = "OWNER" | "ADMIN" | "MEMBER";

export type OrganizationMemberStatus = "ACTIVE" | "SUSPENDED" | "REMOVED";

export type TrackKind = "AUDIO" | "VIDEO" | "SCREEN_SHARE";

export type ChatSendMode = "EVERYONE" | "HOSTS_ONLY";

export type PrivateChatMode = "EVERYONE" | "HOSTS_ONLY" | "DISABLED";

export type ScreenSharePolicy = "HOST_ONLY" | "PRESENTERS_ONLY" | "ALL_INTERACTIVE";

export type ConsentKind = "RECORDING" | "TRANSCRIPTION" | "AI_PROCESSING" | "DATA_SHARING" | "HEALTHCARE_TERMS" | "PRIVACY_NOTICE";

export type ArtifactType =
  | "RECORDING"
  | "TRANSCRIPT"
  | "MEETING_SUMMARY"
  | "ATTENDANCE_REPORT"
  | "POLL_REPORT"
  | "CHAT_EXPORT";

export type ArtifactStatus = "REQUESTED" | "PROCESSING" | "READY" | "FAILED" | "DELETED";

export type PollStatus = "DRAFT" | "OPEN" | "CLOSED" | "ARCHIVED";

export type PollType = "SINGLE_CHOICE" | "MULTIPLE_CHOICE" | "SHORT_TEXT" | "LONG_TEXT" | "RATING" | "RANKING";

export type ResultsVisibility = "HOST_ONLY" | "AFTER_VOTE" | "AFTER_CLOSE";

export type QuestionStatus = "OPEN" | "ANSWERED" | "DISMISSED" | "ARCHIVED";

// ---------------------------------------------------------------------------
// Meeting Spaces
// ---------------------------------------------------------------------------

export interface AccessConfig {
  allowedDomains?: string[];
  adapterRef?: string;
  appointmentRef?: string;
  requireVerifiedRevocation?: boolean;
  waitingRoomAdapterRef?: string;
}

export interface SchedulePolicy {
  earlyStartMinutes?: number;
  lateStartGraceMinutes?: number;
  autoEnd?: boolean;
  autoEndGraceMinutes?: number;
}

export interface RolePolicy {
  overrides?: Partial<Record<Role, Record<string, boolean>>>;
}

export interface CollaborationPolicyPatch {
  screenSharePolicy?: ScreenSharePolicy;
  audioLocked?: boolean;
  videoLocked?: boolean;
  maxSpotlights?: number;
  chatEnabled?: boolean;
  chatSendMode?: ChatSendMode;
  privateChatMode?: PrivateChatMode;
  breakoutChatEnabled?: boolean;
  mainChatVisibleInBreakouts?: boolean;
  reactionsEnabled?: boolean;
  allowedReactions?: string[];
  handRaiseEnabled?: boolean;
  qnaEnabled?: boolean;
  qnaAnonymousAllowed?: boolean;
  pollsEnabled?: boolean;
  maxBreakouts?: number;
  breakoutRecordingPolicy?: "INHERIT_MAIN_POLICY" | "DISABLED" | "SEPARATE_ALLOWED";
  requiredConsents?: ConsentKind[];
  consentVersion?: string;
  aiNotice?: boolean;
}

export interface CollaborationPolicy {
  screenSharePolicy: ScreenSharePolicy;
  audioLocked: boolean;
  videoLocked: boolean;
  maxSpotlights: number;
  chatEnabled: boolean;
  chatSendMode: ChatSendMode;
  privateChatMode: PrivateChatMode;
  breakoutChatEnabled: boolean;
  mainChatVisibleInBreakouts: boolean;
  reactionsEnabled: boolean;
  allowedReactions: string[];
  handRaiseEnabled: boolean;
  qnaEnabled: boolean;
  qnaAnonymousAllowed: boolean;
  pollsEnabled: boolean;
  maxBreakouts: number;
  breakoutRecordingPolicy: "INHERIT_MAIN_POLICY" | "DISABLED" | "SEPARATE_ALLOWED";
  requiredConsents: ConsentKind[];
  consentVersion: string;
  aiNotice: boolean;
}

export interface CreateMeetingSpaceRequest {
  title: string;
  description?: string;
  meetingType?: MeetingType;
  timezone?: string;
  accessPolicy?: AccessPolicy;
  waitingRoomPolicy?: WaitingRoomPolicy;
  securityProfile?: SecurityProfile;
  defaultRole?: Role;
  passcode?: string;
  interactiveCapacity?: number;
  viewerCapacity?: number;
  recordingPolicy?: MediaCapturePolicy;
  transcriptionPolicy?: MediaCapturePolicy;
  retentionPolicyRef?: string;
  organizationId?: string;
  accessConfig?: AccessConfig;
  rolePolicy?: RolePolicy;
  schedulePolicy?: SchedulePolicy;
  collaborationPolicy?: CollaborationPolicyPatch;
}

export interface UpdateMeetingSpaceRequest {
  version: number;
  title?: string;
  description?: string;
  meetingType?: MeetingType;
  timezone?: string;
  accessPolicy?: AccessPolicy;
  waitingRoomPolicy?: WaitingRoomPolicy;
  securityProfile?: SecurityProfile;
  defaultRole?: Role;
  passcode?: string;
  clearPasscode?: boolean;
  interactiveCapacity?: number;
  viewerCapacity?: number;
  recordingPolicy?: MediaCapturePolicy;
  transcriptionPolicy?: MediaCapturePolicy;
  retentionPolicyRef?: string;
  accessConfig?: AccessConfig;
  rolePolicy?: RolePolicy;
  schedulePolicy?: SchedulePolicy;
  collaborationPolicy?: CollaborationPolicyPatch;
}

export interface VersionedRequest {
  version?: number;
}

export interface DuplicateMeetingSpaceRequest {
  title?: string;
}

export interface OwnerTransferRequest {
  newOwnerUserId: string;
  version: number;
  retainPreviousOwner?: boolean;
}

export interface InstantMeetingRequestV2 {
  title?: string;
  timezone?: string;
  accessPolicy?: AccessPolicy;
  waitingRoomPolicy?: WaitingRoomPolicy;
  passcode?: string;
}

/** Minimal conference info included in space responses. */
export interface ConferenceSummary {
  id: string;
  status: ConferenceStatus;
  scheduledStartAt?: string;
  scheduledEndAt?: string;
  startedAt?: string;
  endedAt?: string;
}

export interface MeetingSpaceResponse {
  id: string;
  meetingCode: string;
  view: ResourceView;
  title?: string;
  meetingType: MeetingType;
  status: SpaceStatus;
  accessPolicy: AccessPolicy;
  requiresPasscode: boolean;
  isLegacy: boolean;
  viewerRole?: Role;
  description?: string;
  timezone?: string;
  waitingRoomPolicy?: WaitingRoomPolicy;
  securityProfile?: SecurityProfile;
  defaultRole?: Role;
  interactiveCapacity?: number;
  viewerCapacity?: number;
  recordingPolicy?: MediaCapturePolicy;
  transcriptionPolicy?: MediaCapturePolicy;
  retentionPolicyRef?: string;
  organizationId?: string;
  ownerUserId?: string;
  accessConfig?: AccessConfig;
  rolePolicy?: RolePolicy;
  schedulePolicy?: SchedulePolicy;
  collaborationPolicy?: CollaborationPolicyPatch;
  createdAt?: string;
  updatedAt?: string;
  archivedAt?: string;
  version?: number;
  /** The canonical conference for this space (live > upcoming > latest). */
  latestConference?: ConferenceSummary;
}

export interface MeetingSpaceListResponse {
  items: MeetingSpaceResponse[];
}

export interface InstantMeetingResponseV2 {
  meetingSpace: MeetingSpaceResponse;
  conference: ConferenceResponse;
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

export interface AddMemberRequest {
  userId?: string;
  email?: string;
  role: Role;
}

export interface UpdateMemberRequest {
  role: Role;
  version: number;
}

export interface MemberResponse {
  id: string;
  meetingSpaceId: string;
  principalType: PrincipalType;
  userId?: string;
  email?: string;
  role: Role;
  status: MemberStatus;
  createdAt: string;
  updatedAt: string;
  revokedAt?: string;
  version: number;
}

export interface MemberListResponse {
  items: MemberResponse[];
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

export interface CreateInvitationRequest {
  role?: Role;
  conferenceId?: string;
  inviteeEmail?: string;
  expiresInHours?: number;
  useLimit?: number;
}

export interface InvitationResponse {
  id: string;
  meetingSpaceId: string;
  conferenceId?: string;
  requestedRole: Role;
  inviteeEmail?: string;
  expiresAt: string;
  useLimit?: number;
  usageCount: number;
  status: InvitationStatus;
  createdAt: string;
}

export interface CreateInvitationResponse {
  invitation: InvitationResponse;
  token: string;
}

export interface ResolveInvitationResponse {
  meetingSpace: MeetingSpaceResponse;
  conferenceId?: string;
  requestedRole: Role;
  expiresAt: string;
  remainingUses?: number;
}

// ---------------------------------------------------------------------------
// Conferences
// ---------------------------------------------------------------------------

export interface CreateConferenceRequest {
  scheduledStartAt?: string;
  scheduledEndAt?: string;
}

export interface RescheduleConferenceRequest {
  scheduledStartAt: string;
  scheduledEndAt?: string;
}

export interface ListConferencesQuery {
  from?: string;
  to?: string;
}

export interface ConferenceResponse {
  id: string;
  meetingSpaceId: string;
  status: ConferenceStatus;
  view: ResourceView;
  kind?: ConferenceKind;
  occurrenceKey?: string;
  scheduledStartAt?: string;
  scheduledEndAt?: string;
  startedAt?: string;
  endingRequestedAt?: string;
  endedAt?: string;
  terminationReason?: TerminationReason;
  isLocked?: boolean;
  lockedAt?: string;
  isException?: boolean;
  originalStartAt?: string;
  isLegacy?: boolean;
  version?: number;
  recordingState?: string;
  transcriptionState?: string;
}

export interface ConferenceListResponse {
  items: ConferenceResponse[];
}

export interface ConferenceActionResponse {
  conference: ConferenceResponse;
  changed: boolean;
  providerEnforcement: ProviderEnforcement;
}

// ---------------------------------------------------------------------------
// Recurrence
// ---------------------------------------------------------------------------

export interface SetRecurrenceRequest {
  rrule: string;
  startLocal: string;
  durationMinutes: number;
}

export interface RecurrenceResponse {
  id: string;
  meetingSpaceId: string;
  rrule: string;
  timezone: string;
  startLocal: string;
  durationMinutes: number;
  status: RecurrenceStatus;
  materializedUntil?: string;
  version: number;
}

export interface RecurrenceExceptionRequest {
  originalStartAt: string;
  action: "CANCEL" | "RESCHEDULE";
  scheduledStartAt?: string;
  scheduledEndAt?: string;
}

export interface RecurrenceExceptionResponse {
  id: string;
  recurrenceRuleId: string;
  originalStartLocal: string;
  timezone: string;
  kind: RecurrenceExceptionKind;
  newStartAt?: string;
  newEndAt?: string;
  version: number;
  appliedToConferenceId?: string;
}

// ---------------------------------------------------------------------------
// Join / Participants
// ---------------------------------------------------------------------------

export interface ConsentGrant {
  consentType: ConsentKind;
  granted: boolean;
}

export interface JoinConferenceRequestV2 {
  displayName?: string;
  passcode?: string;
  invitationToken?: string;
  previousSessionId?: string;
  externalContext?: string;
  consents?: ConsentGrant[];
}

export interface JoinConferenceSummary {
  id: string;
  status: ConferenceStatus;
  meetingSpaceId: string;
  isLocked: boolean;
}

export interface ParticipantResponse {
  id: string;
  conferenceId: string;
  identityType: IdentityType;
  displayName: string;
  role: Role;
  admissionState: AdmissionState;
  requestedAt: string;
  admittedAt?: string;
  denialMessage?: string;
  denialReasonCode?: string;
  version: number;
}

export interface ParticipantSessionResponse {
  id: string;
  expiresAt?: string;
}

export interface RtcCredentialResponse {
  provider: string;
  url: string;
  credential: string;
  expiresAt: string;
}

export interface JoinPermissions {
  audioPublish: boolean;
  videoPublish: boolean;
  screenSharePublish: boolean;
  dataPublish: boolean;
  subscribe: boolean;
}

export interface JoinPolicy {
  recordingActive: boolean;
  transcriptionActive: boolean;
}

export interface MediaContextView {
  kind?: "MAIN" | "BREAKOUT" | "BACKSTAGE";
  roomType?: "MAIN" | "BREAKOUT";
  breakoutId?: string | null;
  breakoutName?: string;
  visit?: boolean;
}

export interface JoinRealtimeInfo {
  enabled: boolean;
  ticketPath?: string;
  streamPath?: string;
  eventsPath?: string;
  conferencePath: string;
  participantPath: string;
}

export interface JoinConferenceResponseV2 {
  conference: JoinConferenceSummary;
  participant: ParticipantResponse;
  participantSession?: ParticipantSessionResponse;
  participantToken?: string;
  rtc?: RtcCredentialResponse;
  rtcStatus: RtcStatus;
  permissions: JoinPermissions;
  policy: JoinPolicy;
  mediaContext: MediaContextView;
  realtime: JoinRealtimeInfo;
}

export interface LeaveConferenceRequest {
  participantSessionId?: string;
}

export interface LeaveConferenceResponse {
  closedSessions: number;
}

// ---------------------------------------------------------------------------
// Waiting Room & Moderation
// ---------------------------------------------------------------------------

export interface WaitingRoomResponse {
  conferenceId: string;
  participants: ParticipantResponse[];
}

export interface AdmitRequest {
  participantIds?: string[];
  all?: boolean;
}

export interface DenyRequest {
  participantIds?: string[];
  all?: boolean;
  reasonCode?: string;
  message?: string;
  internalNote?: string;
}

export interface SkippedParticipant {
  participantId: string;
  admissionState: AdmissionState;
}

export interface BulkAdmissionResponse {
  changed: string[];
  unchanged: string[];
  skipped: SkippedParticipant[];
}

export interface RoleChangeRequest {
  role: Role;
  version?: number;
}

export interface RemoveParticipantRequest {
  reason?: string;
}

export interface BanParticipantRequest {
  scope?: BanScope;
  reasonCode?: string;
  reason?: string;
  expiresAt?: string;
}

export interface ModerationResponse {
  participant: ParticipantResponse;
  banId?: string;
  providerEnforcement: ProviderEnforcement;
}

export interface MuteRequest {
  trackKind: TrackKind;
}

export interface MuteResponse {
  participantId: string;
  trackKind: TrackKind;
  sessions: number;
  mutedTracks: number;
}

// ---------------------------------------------------------------------------
// Resolve
// ---------------------------------------------------------------------------

export interface ResolveResponse {
  meetingSpace: MeetingSpaceResponse;
  conference?: ConferenceResponse | null;
  matchedBy: string;
}

// ---------------------------------------------------------------------------
// Realtime
// ---------------------------------------------------------------------------

export interface RealtimeTicketResponse {
  ticket: string;
  expiresAt: string;
  websocketPath: string;
  cursor: number;
  scope: string;
}

export interface EventReplayQuery {
  after?: number;
}

export interface RealtimeEnvelope {
  eventId: string;
  eventType: string;
  conferenceId?: string;
  participantId?: string;
  timestamp: string;
  sequence: number;
  payload: Record<string, unknown>;
}

export interface EventReplayResponse {
  events: RealtimeEnvelope[];
  cursor: number;
  resyncRequired: boolean;
  canonical: string;
}

// ---------------------------------------------------------------------------
// Media Controls & Collaboration
// ---------------------------------------------------------------------------

export interface MediaLockResponse {
  participantId: string;
  audioLocked: boolean;
  videoLocked: boolean;
  screenShareLocked: boolean;
  changed: boolean;
  permissions: JoinPermissions;
  providerEnforcement: ProviderEnforcement;
}

export interface StopScreenShareResponse {
  participantId: string;
  stoppedSessions: number;
}

export interface SpotlightRequest {
  participantId: string;
}

export interface SpotlightView {
  participantId: string;
  displayName: string;
  spotlightedAt: string;
}

export interface SpotlightListResponse {
  changed: boolean;
  spotlights: SpotlightView[];
}

export interface CollaborationPolicyUpdateRequest {
  collaborationVersion: number;
  changes: CollaborationPolicyPatch;
}

export interface CollaborationPolicyResponse {
  conferenceId: string;
  collaborationVersion: number;
  policy: CollaborationPolicy;
  providerEnforcement: ProviderEnforcement;
}

export interface MyCollaborationState {
  participantId: string;
  role: Role;
  permissions: JoinPermissions;
  audioLocked: boolean;
  videoLocked: boolean;
  screenShareLocked: boolean;
  chatRestricted: boolean;
  handRaisedAt?: string;
  mediaContext: MediaContextView;
}

export interface RaisedHand {
  participantId: string;
  displayName: string;
  raisedAt: string;
}

export interface CollaborationStateResponse {
  conferenceId: string;
  collaborationVersion: number;
  eventSequence: number;
  me?: MyCollaborationState;
  policy: CollaborationPolicy;
  spotlights: SpotlightView[];
  raisedHands: RaisedHand[];
  activeScreenShares: string[];
}

export interface MediaCredentialRequest {
  participantSessionId: string;
}

export interface MediaCredentialResponse {
  rtc: RtcCredentialResponse;
  mediaContext: MediaContextView;
  permissions: JoinPermissions;
}

export interface PreJoinResponse {
  meetingSpace: MeetingSpaceResponse;
  conference?: ConferenceResponse;
  requiresAuth: boolean;
  requiresPasscode: boolean;
  requiresInvitation: boolean;
  waitingRoomEnabled: boolean;
  consentRequirements: ConsentRequirement[];
}

export interface ConsentRequirement {
  consentType: ConsentKind;
  required: boolean;
  description?: string;
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

export interface MessageListQuery {
  scope?: "MAIN" | "BREAKOUT" | "PRIVATE";
  breakoutId?: string;
  peerId?: string;
  before?: string;
  limit?: number;
}

export interface MessageView {
  id: string;
  conferenceId: string;
  breakoutId?: string | null;
  senderParticipantId: string;
  senderDisplayName: string;
  recipientParticipantId?: string | null;
  kind: "MESSAGE" | "ANNOUNCEMENT" | "BREAKOUT_ANNOUNCEMENT";
  body?: string | null;
  deleted: boolean;
  createdAt: string;
}

export interface MessagePage {
  messages: MessageView[];
  nextCursor?: string | null;
}

export interface SendMessageRequest {
  body: string;
  breakoutId?: string;
  recipientParticipantId?: string;
  announcement?: boolean;
}

export interface ChatPolicyUpdateRequest {
  collaborationVersion: number;
  chatEnabled?: boolean;
  chatSendMode?: ChatSendMode;
  privateChatMode?: PrivateChatMode;
  breakoutChatEnabled?: boolean;
  mainChatVisibleInBreakouts?: boolean;
}

export interface ChangedResponse {
  changed: boolean;
}

// ---------------------------------------------------------------------------
// Reactions & Hand Raise
// ---------------------------------------------------------------------------

export interface ReactionRequest {
  reaction: string;
}

export interface ReactionResponse {
  reaction: string;
  delivered: boolean;
}

export interface HandRaiseResponse {
  participantId: string;
  raised: boolean;
  raisedAt?: string;
  changed: boolean;
}

export interface RaisedHandsResponse {
  hands: RaisedHand[];
}

// ---------------------------------------------------------------------------
// Breakouts
// ---------------------------------------------------------------------------

export interface BreakoutView {
  id: string;
  conferenceId: string;
  name: string;
  position: number;
  status: "CREATED" | "OPEN" | "CLOSING" | "CLOSED";
  capacity?: number | null;
  recordingPolicy: string;
  participantCount: number;
  participantIds?: string[];
  openedAt?: string;
  closingAt?: string | null;
  closedAt?: string;
  version: number;
}

export interface BreakoutListResponse {
  breakouts: BreakoutView[];
  myContext?: MediaContextView;
}

export interface CreateBreakoutsRequest {
  names?: string[];
  count?: number;
  capacity?: number;
  recordingPolicy?: "INHERIT_MAIN_POLICY" | "DISABLED" | "SEPARATE_ALLOWED";
}

export interface BreakoutListCreatedResponse {
  breakouts: BreakoutView[];
}

export interface AssignBreakoutsRequest {
  mode?: "MANUAL" | "RANDOM" | "BALANCED";
  assignments?: { participantId: string; breakoutId: string }[];
  breakoutIds?: string[];
  includeHosts?: boolean;
  includeViewers?: boolean;
  onlyUnassigned?: boolean;
}

export interface AssignmentResult {
  assigned: { participantId: string; breakoutId: string }[];
  unassigned: string[];
  enforcement: string;
}

export interface MoveParticipantRequest {
  participantId: string;
  toBreakoutId: string | null;
}

export interface MediaContextResponse {
  participantId?: string;
  mediaContext: MediaContextView;
  changed: boolean;
  providerEnforcement: ProviderEnforcement;
}

export interface RoomAssignmentsPatch {
  add?: string[];
  remove?: string[];
}

export interface BreakoutResponse {
  breakout: BreakoutView;
  changed: boolean;
  providerEnforcement: ProviderEnforcement;
}

export interface BreakoutAnnouncementRequest {
  message: string;
}

export interface AnnouncementResponse {
  rooms: number;
}

export interface CloseBreakoutRequest {
  countdownSeconds?: number;
}

export interface CloseBreakoutsResponse {
  breakouts: BreakoutView[];
  providerEnforcement: ProviderEnforcement;
}

// ---------------------------------------------------------------------------
// Polls
// ---------------------------------------------------------------------------

export type PollQuestionType = PollType;

export interface PollOption {
  id: string;
  label: string;
}

export interface PollQuestionInput {
  type: PollQuestionType;
  prompt: string;
  options?: string[];
  required?: boolean;
  minSelections?: number;
  maxSelections?: number;
  maxLength?: number;
  ratingMin?: number;
  ratingMax?: number;
  correct?: number[];
  points?: number;
}

export interface CreatePollRequest {
  title: string;
  questions: PollQuestionInput[];
  anonymous?: boolean;
  quizMode?: boolean;
  resultsVisibility?: ResultsVisibility;
}

export interface PollQuestion extends Omit<PollQuestionInput, "correct" | "options"> {
  id: string;
  options: PollOption[];
  correct?: string[];
  points?: number;
}

export interface PollView {
  id: string;
  conferenceId: string;
  title: string;
  status: PollStatus;
  anonymous: boolean;
  quizMode: boolean;
  resultsVisibility: ResultsVisibility;
  questions: PollQuestion[];
  responseCount: number;
  hasResponded: boolean;
  createdAt: string;
  openedAt?: string | null;
  closedAt?: string | null;
  archivedAt?: string | null;
  version: number;
}

export interface PollListResponse {
  polls: PollView[];
}

export interface PollTransitionResponse {
  poll: PollView;
  changed: boolean;
}

export interface SubmitPollResponseRequest {
  answers: {
    questionId: string;
    optionIds?: string[];
    text?: string;
    rating?: number;
    ranking?: string[];
  }[];
  idempotencyKey?: string;
}

export interface PollResponseReceipt {
  pollId: string;
  recorded: boolean;
  idempotentReplay: boolean;
  score?: number;
  maxScore?: number;
}

export interface PollQuestionResult {
  questionId: string;
  type: PollQuestionType;
  answered: number;
  optionCounts?: Record<string, number>;
  ratingDistribution?: Record<string, number>;
  ratingAverage?: number;
  averagePositions?: Record<string, number>;
  texts?: { text: string; participantId?: string }[];
  correct?: string[];
}

export interface PollResults {
  pollId: string;
  status: PollStatus;
  anonymous: boolean;
  responseCount: number;
  questions: PollQuestionResult[];
  averageScore?: number;
  maxScore?: number;
  myScore?: number;
}

// ---------------------------------------------------------------------------
// Q&A
// ---------------------------------------------------------------------------

export interface SubmitQuestionRequest {
  body: string;
  anonymous?: boolean;
}

export interface QuestionView {
  id: string;
  conferenceId: string;
  body: string;
  participantId?: string;
  displayName?: string;
  anonymous: boolean;
  status: QuestionStatus;
  upvoteCount: number;
  votedByMe: boolean;
  isMine: boolean;
  pinned: boolean;
  answer?: string | null;
  answeredAt?: string | null;
  createdAt: string;
  version: number;
}

export interface QuestionListResponse {
  questions: QuestionView[];
}

export interface QuestionActionResponse {
  question: QuestionView;
  changed: boolean;
}

export interface AnswerQuestionRequest {
  answer: string;
}

// ---------------------------------------------------------------------------
// Recordings & Artifacts
// ---------------------------------------------------------------------------

export interface StartRecordingRequest {
  layout?: string;
  outputFormat?: string;
}

export interface RecordingResponse {
  recording: ArtifactView;
  created: boolean;
}

export interface RecordingListResponse {
  recordings: ArtifactView[];
}

export interface ArtifactView {
  id: string;
  conferenceId: string;
  artifactType: ArtifactType;
  status: ArtifactStatus;
  filename?: string;
  mimeType?: string;
  sizeBytes?: number;
  durationSeconds?: number;
  createdAt: string;
  readyAt?: string;
  deletedAt?: string;
}

export interface ArtifactListQuery {
  artifactType?: ArtifactType;
}

export interface ArtifactListResponse {
  artifacts: ArtifactView[];
}

export interface ArtifactAccess {
  artifactId: string;
  url: string;
  expiresAt: string;
}

export interface AttendanceEntry {
  participantId: string;
  displayName: string;
  identityType: IdentityType;
  role: Role;
  joinTime: string;
  leaveTime?: string;
  durationSeconds: number;
}

export interface AttendanceReport {
  conferenceId: string;
  startedAt?: string;
  endedAt?: string;
  totalDurationSeconds: number;
  peakParticipants: number;
  uniqueParticipants: number;
  entries: AttendanceEntry[];
}

export interface GenerateReportRequest {
  reportType: "ATTENDANCE_REPORT" | "POLL_REPORT" | "CHAT_EXPORT";
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

export interface ConsentDecision {
  consentType: ConsentKind;
  granted: boolean;
  decidedAt: string;
}

export interface ConsentRequirementsResponse {
  conferenceId: string;
  participantId: string;
  requirements: ConsentRequirement[];
  decisions: ConsentDecision[];
}

export interface RecordConsentRequest {
  consents: ConsentGrant[];
}

export interface WithdrawalResponse {
  consentType: ConsentKind;
  withdrawnAt: string;
  effect: string;
}

// ---------------------------------------------------------------------------
// Transcription & Intelligence
// ---------------------------------------------------------------------------

export interface StartTranscriptionRequest {
  mode?: "LIVE" | "POST_MEETING";
  language?: string;
}

export interface TranscriptionJobView {
  id: string;
  conferenceId: string;
  mode: string;
  status: string;
  language: string;
  artifactId?: string;
  createdAt: string;
  startedAt?: string;
  completedAt?: string;
}

export interface TranscriptionJobListResponse {
  jobs: TranscriptionJobView[];
}

export interface TranscriptQuery {
  after?: string;
  limit?: number;
}

export interface TranscriptSegment {
  id: string;
  speakerId?: string;
  speakerName?: string;
  startTime: number;
  endTime: number;
  text: string;
  confidence: number;
}

export interface TranscriptView {
  artifactId: string;
  conferenceId: string;
  language: string;
  segments: TranscriptSegment[];
  hasMore: boolean;
  cursor?: string;
}

export interface TranslateRequest {
  targetLanguage: string;
}

export interface TranslationResponse {
  originalArtifactId: string;
  translatedArtifactId: string;
  targetLanguage: string;
}

export interface IntelligenceRequest {
  outputType: "MEETING_SUMMARY" | "ACTION_ITEMS" | "KEY_POINTS";
}

// ---------------------------------------------------------------------------
// Quality
// ---------------------------------------------------------------------------

export interface QualitySample {
  timestamp: string;
  audioJitter?: number;
  videoJitter?: number;
  audioPacketLoss?: number;
  videoPacketLoss?: number;
  roundTripTime?: number;
  bitrate?: number;
}

export interface QualitySampleBatch {
  sessionId: string;
  samples: QualitySample[];
}

export interface QualityIngestResponse {
  accepted: number;
  rejected: number;
}

export interface ParticipantQuality {
  participantId: string;
  displayName: string;
  averageScore: number;
  issues: string[];
}

export interface ConferenceQualityView {
  conferenceId: string;
  overallScore: number;
  sampleCount: number;
  participants: ParticipantQuality[];
}

// ---------------------------------------------------------------------------
// Profiles & Registration
// ---------------------------------------------------------------------------

export interface ProfilePolicyResponse {
  meetingSpaceId: string;
  effectivePolicy: CollaborationPolicyPatch;
  spaceOverride?: CollaborationPolicyPatch;
  organizationPolicy?: CollaborationPolicyPatch;
  version: number;
}

export interface ProfilePolicyUpdateRequest {
  version: number;
  policy: CollaborationPolicyPatch;
}

export interface RegistrationRequest {
  email: string;
  displayName: string;
  customFields?: Record<string, string>;
}

export interface RegistrationView {
  id: string;
  meetingSpaceId: string;
  email: string;
  displayName: string;
  status: "PENDING" | "CONFIRMED" | "CANCELLED";
  customFields?: Record<string, string>;
  createdAt: string;
  confirmedAt?: string;
}

export interface RegistrationCreated {
  registration: RegistrationView;
  admissionToken?: string;
  created: boolean;
}

export interface RegistrationListResponse {
  registrations: RegistrationView[];
}

export interface BackstageResponse {
  inBackstage: boolean;
  mediaContext: MediaContextView;
}

// ---------------------------------------------------------------------------
// Organizations
// ---------------------------------------------------------------------------

export interface CreateOrganizationRequest {
  name: string;
  slug: string;
  ownerUserId?: string;
}

export interface UpdateOrganizationRequest {
  status: OrganizationStatus;
}

export interface UpsertOrganizationMemberRequest {
  role: OrganizationRole;
  status: OrganizationMemberStatus;
}

export interface OrganizationResponse {
  id: string;
  name: string;
  slug: string;
  status: OrganizationStatus;
  createdAt: string;
}

export interface OrganizationMemberResponse {
  id: string;
  organizationId: string;
  userId: string;
  role: OrganizationRole;
  status: OrganizationMemberStatus;
  createdAt: string;
  updatedAt: string;
}

export interface MyOrganizationResponse {
  organization: OrganizationResponse;
  role: OrganizationRole;
}

// ---------------------------------------------------------------------------
// Compatibility (Admin)
// ---------------------------------------------------------------------------

export interface ShadowComparisonResponse {
  legacyMeetingId: string;
  meetingSpaceId?: string;
  consistent: boolean;
  differences: string[];
}

export interface BackfillReportResponse {
  legacyMeetingRows: number;
  meetingSpaceRows: number;
  mappedSpaces: number;
  mappingFailures: number;
  legacyConferenceSessions: number;
  legacyParticipantRows: number;
  mappedLegacyParticipantRows: number;
  conferenceParticipantRowsFromLegacy: number;
  participantSessionRowsFromLegacy: number;
  unmappedMeetingIds: string[];
  unmappedParticipantIds: string[];
  insertedSpaces: number;
  insertedConferences: number;
  insertedParticipants: number;
  insertedSessions: number;
}

// ---------------------------------------------------------------------------
// API Response Wrappers
// ---------------------------------------------------------------------------

export interface ApiResponse<T> {
  success: boolean;
  data: T;
}

export interface ApiErrorResponse {
  success: false;
  error: {
    code: string;
    message: string;
    requestId?: string;
  };
}
