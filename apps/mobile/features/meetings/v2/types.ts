/**
 * CF Meet V2 API Types (Mobile)
 * Matches the backend Rust DTOs at services/meeting-service/src/dto/v2.rs
 * All responses use camelCase JSON serialization
 */

// ---------------------------------------------------------------------------
// Common Enums
// ---------------------------------------------------------------------------

export type MeetingType = "STANDARD" | "HEALTHCARE" | "WEBINAR" | "TOWN_HALL" | "TRAINING";

export type SpaceStatus = "ACTIVE" | "ARCHIVED";

export type AccessPolicy =
  | "PUBLIC"
  | "PUBLIC_PASSCODE"
  | "REGISTERED_USERS"
  | "ORGANIZATION_ONLY"
  | "DOMAIN_RESTRICTED"
  | "EXTERNAL_ONLY"
  | "MEMBERS_ONLY"
  | "INVITATION_ONLY"
  | "CUSTOM"
  | "HEALTHCARE_APPOINTMENT";

export type WaitingRoomPolicy = "OFF" | "GUESTS_ONLY" | "EVERYONE" | "CUSTOM";

export type SecurityProfile = "STANDARD" | "HEALTHCARE" | "HIGH_SECURITY";

export type Role = "HOST" | "CO_HOST" | "PRESENTER" | "PARTICIPANT" | "VIEWER";

export type MediaCapturePolicy = "DISABLED" | "HOST_ONLY" | "ALLOWED";

export type ConferenceStatus =
  | "CREATED"
  | "PREPARING"
  | "WAITING"
  | "LIVE"
  | "ENDING"
  | "ENDED"
  | "CANCELLED"
  | "FAILED";

export type ConferenceKind = "INSTANT" | "SCHEDULED" | "RECURRING";

export type TerminationReason =
  | "HOST_ENDED"
  | "SYSTEM_TIMEOUT"
  | "PROVIDER_TERMINATED"
  | "CANCELLED";

export type IdentityType = "REGISTERED" | "GUEST";

export type AdmissionState = "WAITING" | "ADMITTED" | "DENIED" | "LEFT" | "REMOVED" | "BANNED";

export type PrincipalType = "USER" | "EMAIL";

export type MemberStatus = "ACTIVE" | "REVOKED";

export type InvitationStatus = "ACTIVE" | "REVOKED" | "EXPIRED" | "EXHAUSTED";

export type ResourceView = "FULL" | "SUMMARY";

export type ProviderEnforcement = "PENDING_PROVIDER_CONTROL" | "ENFORCED" | "NOT_REQUIRED";

export type RtcStatus = "ISSUED" | "NOT_ISSUED_WAITING" | "PROVIDER_CONTROL_DISABLED";

export type TrackKind = "AUDIO" | "VIDEO" | "SCREEN_SHARE";

export type ChatSendMode = "EVERYONE" | "HOST_ONLY" | "DISABLED";

export type PrivateChatMode = "ENABLED" | "HOSTS_ONLY" | "DISABLED";

export type ScreenShareMode = "HOST_ONLY" | "PRESENTERS_AND_ABOVE" | "EVERYONE" | "DISABLED";

export type ConsentKind = "RECORDING" | "TRANSCRIPTION" | "AI_PROCESSING" | "DATA_SHARING";

export type PollStatus = "DRAFT" | "OPEN" | "CLOSED" | "ARCHIVED";

export type PollType = "SINGLE_CHOICE" | "MULTIPLE_CHOICE" | "QUIZ";

export type QuestionStatus = "PENDING" | "ANSWERED" | "DISMISSED" | "ARCHIVED";

export type BreakoutStatus = "PENDING" | "OPEN" | "CLOSING" | "CLOSED";

// ---------------------------------------------------------------------------
// Meeting Spaces
// ---------------------------------------------------------------------------

export interface AccessConfig {
  allowedDomains?: string[];
  adapterRef?: string;
  appointmentRef?: string;
}

export interface SchedulePolicy {
  earlyStartMinutes?: number;
  lateStartGraceMinutes?: number;
  autoEnd?: boolean;
  autoEndGraceMinutes?: number;
}

export interface RolePolicy {
  hostCanUnmuteOthers?: boolean;
  participantCanUnmute?: boolean;
  participantCanShareScreen?: boolean;
  participantCanChat?: boolean;
  viewerCanChat?: boolean;
  viewerCanReact?: boolean;
}

export interface CollaborationPolicyPatch {
  audioLockDefault?: boolean;
  videoLockDefault?: boolean;
  screenShareMode?: ScreenShareMode;
  chatEnabled?: boolean;
  chatSendMode?: ChatSendMode;
  privateChatMode?: PrivateChatMode;
  breakoutChatEnabled?: boolean;
  mainChatVisibleInBreakouts?: boolean;
  reactionsEnabled?: boolean;
  handRaiseEnabled?: boolean;
  pollsEnabled?: boolean;
  qnaEnabled?: boolean;
  breakoutsEnabled?: boolean;
  recordingConsentRequired?: boolean;
  transcriptionConsentRequired?: boolean;
  aiProcessingConsentRequired?: boolean;
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
  organizationId?: string;
  accessConfig?: AccessConfig;
  rolePolicy?: RolePolicy;
  schedulePolicy?: SchedulePolicy;
  collaborationPolicy?: CollaborationPolicyPatch;
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

export interface InstantMeetingRequest {
  title?: string;
  timezone?: string;
  accessPolicy?: AccessPolicy;
  waitingRoomPolicy?: WaitingRoomPolicy;
  passcode?: string;
}

export interface InstantMeetingResponse {
  meetingSpace: MeetingSpaceResponse;
  conference: ConferenceResponse;
}

// ---------------------------------------------------------------------------
// Conferences
// ---------------------------------------------------------------------------

export interface CreateConferenceRequest {
  scheduledStartAt?: string;
  scheduledEndAt?: string;
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
// Join / Participants
// ---------------------------------------------------------------------------

export interface ConsentGrant {
  consentType: ConsentKind;
  granted: boolean;
}

export interface JoinConferenceRequest {
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
  roomType: "MAIN" | "BREAKOUT";
  breakoutId?: string;
  breakoutName?: string;
}

export interface JoinRealtimeInfo {
  enabled: boolean;
  ticketPath?: string;
  streamPath?: string;
  eventsPath?: string;
  conferencePath: string;
  participantPath: string;
}

export interface JoinConferenceResponse {
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
  conference?: ConferenceResponse;
  matchedBy: string;
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
  senderId: string;
  senderDisplayName: string;
  content: string;
  scope: string;
  breakoutId?: string;
  recipientId?: string;
  createdAt: string;
  deletedAt?: string;
}

export interface MessagePage {
  messages: MessageView[];
  hasMore: boolean;
  cursor?: string;
}

export interface SendMessageRequest {
  content: string;
  scope?: "MAIN" | "BREAKOUT" | "PRIVATE";
  breakoutId?: string;
  recipientId?: string;
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

export interface RaisedHand {
  participantId: string;
  displayName: string;
  raisedAt: string;
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
  displayOrder: number;
  status: BreakoutStatus;
  participantCount: number;
  assignedParticipants?: string[];
  openedAt?: string;
  closingAt?: string;
  closedAt?: string;
}

export interface BreakoutListResponse {
  breakouts: BreakoutView[];
  myContext?: MediaContextView;
}

export interface CreateBreakoutsRequest {
  names?: string[];
  count?: number;
  autoOpen?: boolean;
}

export interface BreakoutListCreatedResponse {
  breakouts: BreakoutView[];
}

export interface AssignBreakoutsRequest {
  mode: "MANUAL" | "RANDOM" | "BALANCED";
  assignments?: Record<string, string[]>;
}

export interface AssignmentResult {
  assignments: Record<string, string[]>;
  moved: number;
}

export interface MoveParticipantRequest {
  participantId: string;
  targetBreakoutId?: string;
}

export interface MediaContextResponse {
  participantId?: string;
  mediaContext: MediaContextView;
  changed: boolean;
  providerEnforcement: ProviderEnforcement;
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

export interface PollOption {
  id: string;
  text: string;
  isCorrect?: boolean;
}

export interface CreatePollRequest {
  pollType: PollType;
  question: string;
  options: Omit<PollOption, "id">[];
  allowMultiple?: boolean;
  showResultsMode?: "ALWAYS" | "AFTER_CLOSE" | "NEVER";
  isAnonymous?: boolean;
}

export interface PollView {
  id: string;
  conferenceId: string;
  pollType: PollType;
  question: string;
  options: PollOption[];
  status: PollStatus;
  allowMultiple: boolean;
  showResultsMode: string;
  isAnonymous: boolean;
  responseCount: number;
  createdAt: string;
  openedAt?: string;
  closedAt?: string;
}

export interface PollListResponse {
  polls: PollView[];
}

export interface PollTransitionResponse {
  poll: PollView;
  changed: boolean;
}

export interface SubmitPollResponseRequest {
  optionIds: string[];
}

export interface PollResponseReceipt {
  pollId: string;
  participantId: string;
  submittedAt: string;
  idempotentReplay: boolean;
}

export interface PollOptionResult {
  optionId: string;
  text: string;
  count: number;
  percentage: number;
  isCorrect?: boolean;
}

export interface PollResults {
  pollId: string;
  totalResponses: number;
  options: PollOptionResult[];
  myResponse?: string[];
}

// ---------------------------------------------------------------------------
// Q&A
// ---------------------------------------------------------------------------

export interface SubmitQuestionRequest {
  content: string;
  isAnonymous?: boolean;
}

export interface QuestionView {
  id: string;
  conferenceId: string;
  content: string;
  askerId?: string;
  askerDisplayName?: string;
  isAnonymous: boolean;
  status: QuestionStatus;
  upvotes: number;
  hasMyVote: boolean;
  isPinned: boolean;
  answer?: string;
  answeredBy?: string;
  answeredAt?: string;
  createdAt: string;
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
// Collaboration State
// ---------------------------------------------------------------------------

export interface SpotlightView {
  participantId: string;
  displayName: string;
  spotlightedAt: string;
}

export interface SpotlightRequest {
  participantId: string;
}

export interface SpotlightListResponse {
  changed: boolean;
  spotlights: SpotlightView[];
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

export interface CollaborationStateResponse {
  conferenceId: string;
  collaborationVersion: number;
  eventSequence: number;
  me?: MyCollaborationState;
  policy: CollaborationPolicyPatch;
  spotlights: SpotlightView[];
  raisedHands: RaisedHand[];
  activeScreenShares: string[];
}

export interface MediaLockResponse {
  participantId: string;
  audioLocked: boolean;
  videoLocked: boolean;
  screenShareLocked: boolean;
  changed: boolean;
  permissions: JoinPermissions;
  providerEnforcement: ProviderEnforcement;
}

// ---------------------------------------------------------------------------
// Participants List
// ---------------------------------------------------------------------------

export interface ParticipantListResponse {
  participants: ParticipantResponse[];
}

// ---------------------------------------------------------------------------
// API Response Wrapper
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
