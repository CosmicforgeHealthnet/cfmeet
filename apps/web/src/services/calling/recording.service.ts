/**
 * Call Recording & Consent API service.
 * Manages recording permissions, consent, and recording lifecycle.
 */

import callingApi from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import type {
  RecordingAuthorizationList,
  CallRecordingList,
  RecordConsentRequest,
  RecordConsentResult,
  StartRecordingRequest,
  StartRecordingResult,
  StopRecordingResult,
} from "@/types/calling";

export const recordingService = {
  /**
   * Get recording authorization decisions for a call
   */
  getAuthorizations: async (callId: string): Promise<RecordingAuthorizationList> => {
    const response = await callingApi.get<RecordingAuthorizationList>(
      `/v1/calls/${encodeURIComponent(callId)}/recording-authorizations`
    );
    return response.data;
  },

  /**
   * Record a participant's consent decision
   */
  recordConsent: async (
    callId: string,
    body: RecordConsentRequest
  ): Promise<RecordConsentResult> => {
    const response = await callingApi.post<RecordConsentResult>(
      `/v1/calls/${encodeURIComponent(callId)}/recording-consent`,
      body
    );
    return response.data;
  },

  /**
   * List recordings for a call
   */
  listRecordings: async (callId: string): Promise<CallRecordingList> => {
    const response = await callingApi.get<CallRecordingList>(
      `/v1/calls/${encodeURIComponent(callId)}/recordings`
    );
    return response.data;
  },

  /**
   * Start recording a call
   */
  startRecording: async (
    callId: string,
    body: StartRecordingRequest = {},
    idempotencyKey?: string
  ): Promise<StartRecordingResult> => {
    const response = await callingApi.post<StartRecordingResult>(
      `/v1/calls/${encodeURIComponent(callId)}/recordings`,
      body,
      {
        headers: {
          "Idempotency-Key": idempotencyKey ?? newIdempotencyKey(),
        },
      }
    );
    return response.data;
  },

  /**
   * Stop an active recording
   */
  stopRecording: async (
    callId: string,
    recordingId: string
  ): Promise<StopRecordingResult> => {
    const response = await callingApi.post<StopRecordingResult>(
      `/v1/calls/${encodeURIComponent(callId)}/recordings/${encodeURIComponent(recordingId)}/stop`,
      {}
    );
    return response.data;
  },
};
