"use client";

/**
 * React Query hooks for call recording and consent management.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { recordingService } from "@/services/calling";
import { callingKeys } from "./useCalls";
import type { RecordConsentRequest, StartRecordingRequest } from "@/types/calling";

export const recordingKeys = {
  all: ["calling", "recordings"] as const,
  authorizations: (callId: string) =>
    [...recordingKeys.all, callId, "authorizations"] as const,
  recordings: (callId: string) =>
    [...recordingKeys.all, callId, "list"] as const,
};

/**
 * Get recording authorization decisions for a call
 */
export function useRecordingAuthorizations(callId: string) {
  return useQuery({
    queryKey: recordingKeys.authorizations(callId),
    queryFn: () => recordingService.getAuthorizations(callId),
    enabled: !!callId,
    refetchInterval: 5000,
  });
}

/**
 * List recordings for a call
 */
export function useCallRecordings(callId: string) {
  return useQuery({
    queryKey: recordingKeys.recordings(callId),
    queryFn: () => recordingService.listRecordings(callId),
    enabled: !!callId,
    refetchInterval: 5000,
  });
}

/**
 * Record a participant's consent decision
 */
export function useRecordConsent(callId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: RecordConsentRequest) =>
      recordingService.recordConsent(callId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: recordingKeys.authorizations(callId),
      });
    },
  });
}

/**
 * Start recording a call
 */
export function useStartRecording(callId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data?: StartRecordingRequest) =>
      recordingService.startRecording(callId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: recordingKeys.recordings(callId),
      });
      queryClient.invalidateQueries({
        queryKey: callingKeys.detail(callId),
      });
    },
  });
}

/**
 * Stop an active recording
 */
export function useStopRecording(callId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (recordingId: string) =>
      recordingService.stopRecording(callId, recordingId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: recordingKeys.recordings(callId),
      });
      queryClient.invalidateQueries({
        queryKey: callingKeys.detail(callId),
      });
    },
  });
}
