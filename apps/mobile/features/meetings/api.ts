/**
 * Meeting Service v2 operations used by the mobile app (MTG-001 / MTG-002 / MTG-003).
 *
 * Every call goes through `send(ROUTES.x, …)`: canonical v2 paths only, request and response
 * bodies typed by the generated contract DTOs. Mobile performs no v1 meeting write (FE-022).
 */
import { send } from "@/lib/api/client";
import { ROUTES } from "@/lib/api/routes";
import type {
  AdmitRequest,
  AgendaResponse,
  BulkAdmissionResponse,
  ConferenceActionResponse,
  ConferenceListResponse,
  ConferenceResponse,
  CreateConferenceRequest,
  CreateMeetingSpaceRequest,
  DenyRequest,
  HandRaiseResponse,
  InstantMeetingRequest,
  InstantMeetingResponse,
  JoinConferenceRequest,
  JoinConferenceResponse,
  LeaveConferenceRequest,
  LeaveConferenceResponse,
  MeetingSpaceListResponse,
  MeetingSpaceResponse,
  MessagePage,
  MessageView,
  MuteRequest,
  MuteResponse,
  ParticipantActionResponse,
  ParticipantResponse,
  PreJoinResponse,
  RaisedHandsResponse,
  ReactionResponse,
  RemoveParticipantRequest,
  RescheduleConferenceRequest,
  ResolveResponse,
  SendMessageRequest,
  UpdateMeetingSpaceRequest,
  VersionedRequest,
  WaitingRoomResponse,
} from "@/lib/api/generated/meeting-service";

/** A random idempotency key for creating requests (retries must not create duplicates). */
export function idempotencyKey(): string {
  const bytes = new Array(16).fill(0).map(() => Math.floor(Math.random() * 256));
  return `mobile-${bytes.map((b) => b.toString(16).padStart(2, "0")).join("")}`;
}

export interface AgendaParams {
  from: string;
  to: string;
  organizationId?: string;
  cursor?: string;
  limit?: number;
}

export const meetingsApi = {
  // ---- Calendar / agenda --------------------------------------------------------------------
  agenda: (p: AgendaParams) =>
    send<AgendaResponse>(ROUTES.myConferences, [], {
      params: { from: p.from, to: p.to, organizationId: p.organizationId, cursor: p.cursor, limit: p.limit ?? 100 },
    }),

  // ---- Meeting spaces -----------------------------------------------------------------------
  listSpaces: (cursor?: string) =>
    send<MeetingSpaceListResponse>(ROUTES.listSpaces, [], { params: { cursor, limit: 100 } }),
  getSpace: (id: string) => send<MeetingSpaceResponse>(ROUTES.getSpace, [id]),
  createSpace: (data: CreateMeetingSpaceRequest, key: string) =>
    send<MeetingSpaceResponse>(ROUTES.createSpace, [], { data, headers: { "Idempotency-Key": key } }),
  updateSpace: (id: string, data: UpdateMeetingSpaceRequest) =>
    send<MeetingSpaceResponse>(ROUTES.updateSpace, [id], { data }),
  archiveSpace: (id: string, data: VersionedRequest) =>
    send<MeetingSpaceResponse>(ROUTES.archiveSpace, [id], { data }),
  listConferences: (spaceId: string, from?: string, to?: string) =>
    send<ConferenceListResponse>(ROUTES.listConferences, [spaceId], { params: { from, to } }),
  createConference: (spaceId: string, data: CreateConferenceRequest, key: string) =>
    send<ConferenceResponse>(ROUTES.createConference, [spaceId], { data, headers: { "Idempotency-Key": key } }),
  instantMeeting: (data: InstantMeetingRequest) => send<InstantMeetingResponse>(ROUTES.instantMeeting, [], { data }),
  /** Public lookup by meeting code, link slug or id (works without an account). */
  resolve: (identifier: string, signedIn: boolean) =>
    send<ResolveResponse>(ROUTES.resolve, [identifier], { anonymous: !signedIn }),

  // ---- Conferences --------------------------------------------------------------------------
  getConference: (id: string) => send<ConferenceResponse>(ROUTES.getConference, [id]),
  startConference: (id: string) => send<ConferenceActionResponse>(ROUTES.startConference, [id]),
  endConference: (id: string) => send<ConferenceActionResponse>(ROUTES.endConference, [id]),
  cancelConference: (id: string) => send<ConferenceActionResponse>(ROUTES.cancelConference, [id]),
  rescheduleConference: (id: string, data: RescheduleConferenceRequest) =>
    send<ConferenceActionResponse>(ROUTES.rescheduleConference, [id], { data }),
  lockConference: (id: string) => send<ConferenceActionResponse>(ROUTES.lockConference, [id]),
  unlockConference: (id: string) => send<ConferenceActionResponse>(ROUTES.unlockConference, [id]),

  // ---- Join / admission ---------------------------------------------------------------------
  preJoin: (id: string) => send<PreJoinResponse>(ROUTES.preJoin, [id]),
  join: (id: string, data: JoinConferenceRequest) => send<JoinConferenceResponse>(ROUTES.join, [id], { data }),
  leave: (id: string, data: LeaveConferenceRequest) => send<LeaveConferenceResponse>(ROUTES.leave, [id], { data }),
  /** Read-only admission state of the caller (polled while waiting; never re-POSTs join). */
  me: (id: string, signal?: AbortSignal) => send<ParticipantResponse>(ROUTES.me, [id], { signal }),

  // ---- Participants and waiting room (host tools) -------------------------------------------
  participants: (id: string) => send<ParticipantResponse[]>(ROUTES.participants, [id]),
  waitingRoom: (id: string) => send<WaitingRoomResponse>(ROUTES.waitingRoom, [id]),
  admit: (id: string, data: AdmitRequest) => send<BulkAdmissionResponse>(ROUTES.admit, [id], { data }),
  deny: (id: string, data: DenyRequest) => send<BulkAdmissionResponse>(ROUTES.deny, [id], { data }),
  removeParticipant: (id: string, participantId: string, data: RemoveParticipantRequest) =>
    send<ParticipantActionResponse>(ROUTES.removeParticipant, [id, participantId], { data }),
  muteParticipant: (id: string, participantId: string, data: MuteRequest) =>
    send<MuteResponse>(ROUTES.muteParticipant, [id, participantId], { data }),

  // ---- In-meeting chat (MSG-012: v2 conference chat) -----------------------------------------
  messages: (id: string, before?: string) =>
    send<MessagePage>(ROUTES.messages, [id], { params: { scope: "MAIN", before, limit: 100 } }),
  sendMessage: (id: string, data: SendMessageRequest) => send<MessageView>(ROUTES.sendMessage, [id], { data }),

  // ---- Engagement ---------------------------------------------------------------------------
  react: (id: string, reaction: string) => send<ReactionResponse>(ROUTES.sendReaction, [id], { data: { reaction } }),
  raisedHands: (id: string) => send<RaisedHandsResponse>(ROUTES.raisedHands, [id]),
  raiseHand: (id: string) => send<HandRaiseResponse>(ROUTES.raiseHand, [id]),
  lowerHand: (id: string) => send<HandRaiseResponse>(ROUTES.lowerHand, [id]),
};
