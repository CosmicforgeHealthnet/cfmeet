/**
 * Participant recording consent and controls (CAL-005) through the same-origin adapter, which
 * binds each request to the signed-in participant (principalId / own endpoint).
 */

import callingApi from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import type {
  CallRecordingEnvelope,
  CallRecordingList,
  RecordConsentRequest,
  RecordingAuthorizationList,
  RecordingConsent,
} from "@/types/calling";

const call = (callId: string) => `/v1/calls/${encodeURIComponent(callId)}`;
const mutation = (key?: string) => ({ headers: { "Idempotency-Key": key ?? newIdempotencyKey() } });

export const recordingService = {
  getAuthorizations: async (callId: string): Promise<RecordingAuthorizationList> =>
    (await callingApi.get<RecordingAuthorizationList>(`${call(callId)}/recording-authorizations`)).data,

  recordConsent: async (callId: string, body: RecordConsentRequest): Promise<RecordingConsent> =>
    (await callingApi.post<RecordingConsent>(`${call(callId)}/recording-consent`, body, mutation())).data,

  listRecordings: async (callId: string): Promise<CallRecordingList> =>
    (await callingApi.get<CallRecordingList>(`${call(callId)}/recordings`)).data,

  startRecording: async (callId: string, idempotencyKey?: string): Promise<CallRecordingEnvelope> =>
    (await callingApi.post<CallRecordingEnvelope>(`${call(callId)}/recordings`, {}, mutation(idempotencyKey))).data,

  stopRecording: async (callId: string, recordingId: string): Promise<CallRecordingEnvelope> =>
    (await callingApi.post<CallRecordingEnvelope>(`${call(callId)}/recordings/${encodeURIComponent(recordingId)}/stop`, {}, mutation())).data,
};
