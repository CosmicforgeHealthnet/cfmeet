/**
 * Call API service (platform credential).
 * Call reads, creation and control, session removal and media credentials.
 */

import callingApi from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { isLiveCallState } from "@/types/calling/common";
import type {
  AcceptResult,
  AvailableActions,
  Call,
  CallListFilters,
  CommandRequest,
  ControlResult,
  CreateCallRequest,
  CreateCallResult,
  DeclineResult,
  InvitationCommandRequest,
  MediaCredential,
  SessionRemovalResult,
  TransferRequest,
  TransferResult,
  UpgradeRequest,
  ScreenShareRequest,
  ScreenShareResult,
  AddParticipantsRequest,
  AddParticipantsResult,
  Uuid,
  CallHistoryFilters,
  CallHistoryList,
  IvrInputRequest,
} from "@/types/calling";

function idempotencyHeader(key?: string): Record<string, string> {
  return { "Idempotency-Key": key ?? newIdempotencyKey() };
}

export const callsService = {
  list: async (filters: CallListFilters = {}): Promise<CallHistoryList> => {
    const response = await callingApi.get<CallHistoryList>("/v1/calls", {
      params: filters,
    });
    return response.data;
  },

  history: async (filters: CallHistoryFilters = {}): Promise<CallHistoryList> => {
    const { data } = await callingApi.get<CallHistoryList>("/v1/calls", { params: filters });
    return data;
  },

  ivrInput: async (callId: Uuid, body: IvrInputRequest): Promise<{ accepted: boolean }> => {
    const { data } = await callingApi.post<{ accepted: boolean }>(`/v1/calls/${callId}/ivr/input`, body);
    return data;
  },

  get: async (callId: Uuid): Promise<Call> => {
    const response = await callingApi.get<Call>(`/v1/calls/${callId}`);
    return response.data;
  },

  actions: async (callId: Uuid): Promise<AvailableActions> => {
    const response = await callingApi.get<AvailableActions>(
      `/v1/calls/${callId}/actions`,
    );
    return response.data;
  },

  create: async (
    data: CreateCallRequest,
    idempotencyKey?: string,
  ): Promise<CreateCallResult> => {
    const response = await callingApi.post<CreateCallResult>("/v1/calls", data, {
      headers: idempotencyHeader(idempotencyKey),
    });
    return response.data;
  },

  accept: async (
    callId: Uuid,
    body: InvitationCommandRequest,
    idempotencyKey?: string,
  ): Promise<AcceptResult> => {
    const response = await callingApi.post<AcceptResult>(
      `/v1/calls/${callId}/accept`,
      body,
      { headers: idempotencyHeader(idempotencyKey) },
    );
    return response.data;
  },

  decline: async (
    callId: Uuid,
    body: InvitationCommandRequest,
    idempotencyKey?: string,
  ): Promise<DeclineResult> => {
    const response = await callingApi.post<DeclineResult>(
      `/v1/calls/${callId}/decline`,
      body,
      { headers: idempotencyHeader(idempotencyKey) },
    );
    return response.data;
  },

  cancel: async (
    callId: Uuid,
    body: CommandRequest,
    idempotencyKey?: string,
  ): Promise<ControlResult> => {
    const response = await callingApi.post<ControlResult>(
      `/v1/calls/${callId}/cancel`,
      body,
      { headers: idempotencyHeader(idempotencyKey) },
    );
    return response.data;
  },

  /**
   * Cancel (still ringing) or end (answered), re-reading the call once when it changed in the
   * meantime (409: it started ringing, or was answered): the other side must stop ringing even
   * if this screen's copy of the call was a moment old.
   */
  hangUp: async (callId: Uuid, operation: "cancel" | "end", expectedVersion: number): Promise<void> => {
    try {
      await callsService[operation](callId, { expectedVersion }, newIdempotencyKey());
    } catch (error) {
      if ((error as { response?: { status?: number } }).response?.status !== 409) throw error;
      const fresh = await callsService.get(callId);
      if (fresh.state === "ENDING" || !isLiveCallState(fresh.state)) return;
      const answered = ["ANSWERED", "CONNECTED", "HELD", "TRANSFERRING"].includes(fresh.state);
      await callsService[answered ? "end" : "cancel"](callId, { expectedVersion: fresh.version }, newIdempotencyKey());
    }
  },

  end: async (
    callId: Uuid,
    body: CommandRequest,
    idempotencyKey?: string,
  ): Promise<ControlResult> => {
    const response = await callingApi.post<ControlResult>(
      `/v1/calls/${callId}/end`,
      body,
      { headers: idempotencyHeader(idempotencyKey) },
    );
    return response.data;
  },

  hold: async (
    callId: Uuid,
    body: CommandRequest,
    idempotencyKey?: string,
  ): Promise<ControlResult> => {
    const response = await callingApi.post<ControlResult>(
      `/v1/calls/${callId}/hold`,
      body,
      { headers: idempotencyHeader(idempotencyKey) },
    );
    return response.data;
  },

  resume: async (
    callId: Uuid,
    body: CommandRequest,
    idempotencyKey?: string,
  ): Promise<ControlResult> => {
    const response = await callingApi.post<ControlResult>(
      `/v1/calls/${callId}/resume`,
      body,
      { headers: idempotencyHeader(idempotencyKey) },
    );
    return response.data;
  },

  transfer: async (
    callId: Uuid,
    body: TransferRequest,
    idempotencyKey?: string,
  ): Promise<TransferResult> => {
    const response = await callingApi.post<TransferResult>(
      `/v1/calls/${callId}/transfer`,
      body,
      { headers: idempotencyHeader(idempotencyKey) },
    );
    return response.data;
  },

  /** C5: VOICE -> VIDEO (never turns a camera on; the camera button becomes available). */
  /** C13: hide a finished call from your own history. */
  hideFromHistory: async (callId: Uuid): Promise<void> => {
    await callingApi.delete(`/v1/calls/${callId}/history`);
  },
  /** C4: the host hands the role to another participant. */
  transferHost: async (callId: Uuid, hostParticipantId: Uuid, idempotencyKey?: string) =>
    (await callingApi.post<ControlResult>(`/v1/calls/${callId}/host`, { hostParticipantId }, { headers: idempotencyHeader(idempotencyKey) })).data,
  /** C4: the host removes or mutes a participant. */
  removeParticipant: async (callId: Uuid, participantId: Uuid, idempotencyKey?: string) =>
    (await callingApi.post<ControlResult>(`/v1/calls/${callId}/participants/${participantId}/remove`, {}, { headers: idempotencyHeader(idempotencyKey) })).data,
  muteParticipant: async (callId: Uuid, participantId: Uuid, muted: boolean, idempotencyKey?: string) =>
    (await callingApi.post<ControlResult>(`/v1/calls/${callId}/participants/${participantId}/mute`, { muted }, { headers: idempotencyHeader(idempotencyKey) })).data,
  /** C6: come back into an ongoing call you left; returns the new session to join. */
  rejoin: async (callId: Uuid, participantId: Uuid, endpointId: Uuid, idempotencyKey?: string) =>
    (await callingApi.post<{ sessionId: Uuid; rejoined: boolean }>(`/v1/calls/${callId}/participants/${participantId}/rejoin`, { endpointId }, { headers: idempotencyHeader(idempotencyKey) })).data,
  /** C8: call quality per participant (estimated MOS 1–5). */
  quality: async (callId: Uuid) =>
    (await callingApi.get<{ participants: { participantId: Uuid; sampleCount: number; mosEstimate: { avg: number; min: number }; packetLossPercent: { avg: number }; rttMs: { avg: number } }[] }>(`/v1/calls/${callId}/quality`)).data,

  upgrade: async (callId: Uuid, body: UpgradeRequest, idempotencyKey?: string): Promise<ControlResult> => {
    const response = await callingApi.post<ControlResult>(`/v1/calls/${callId}/upgrade`, body, { headers: idempotencyHeader(idempotencyKey) });
    return response.data;
  },

  /** C6: become the call's presenter (one per call). */
  startScreenShare: async (callId: Uuid, body: ScreenShareRequest, idempotencyKey?: string): Promise<ScreenShareResult> => {
    const response = await callingApi.post<ScreenShareResult>(`/v1/calls/${callId}/screen-share`, body, { headers: idempotencyHeader(idempotencyKey) });
    return response.data;
  },

  /** C6: stop presenting. */
  stopScreenShare: async (callId: Uuid, body: ScreenShareRequest, idempotencyKey?: string): Promise<ScreenShareResult> => {
    const response = await callingApi.post<ScreenShareResult>(`/v1/calls/${callId}/screen-share/stop`, body, { headers: idempotencyHeader(idempotencyKey) });
    return response.data;
  },

  /** C7: invite people into the ongoing call. */
  addParticipants: async (callId: Uuid, body: AddParticipantsRequest, idempotencyKey?: string): Promise<AddParticipantsResult> => {
    const response = await callingApi.post<AddParticipantsResult>(`/v1/calls/${callId}/participants`, body, { headers: idempotencyHeader(idempotencyKey) });
    return response.data;
  },

  removeSession: async (
    callId: Uuid,
    sessionId: Uuid,
    body: CommandRequest,
    idempotencyKey?: string,
  ): Promise<SessionRemovalResult> => {
    const response = await callingApi.post<SessionRemovalResult>(
      `/v1/calls/${callId}/sessions/${sessionId}/remove`,
      body,
      { headers: idempotencyHeader(idempotencyKey) },
    );
    return response.data;
  },

  issueSessionMediaCredential: async (
    callId: Uuid,
    sessionId: Uuid,
  ): Promise<MediaCredential> => {
    const response = await callingApi.post<MediaCredential>(
      `/v1/calls/${callId}/sessions/${sessionId}/media-credentials`,
      {},
    );
    return response.data;
  },
};
