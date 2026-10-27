/**
 * Call API service (platform credential).
 * Call reads, creation and control, session removal and media credentials.
 */

import callingApi from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
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
