"use client";

/**
 * React Query hooks for participant recording consent and controls (CAL-005).
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { recordingService } from "@/services/calling";
import { callingKeys } from "./useCalls";
import type { RecordConsentRequest } from "@/types/calling";

export const recordingKeys = {
  all: ["calling", "recordings"] as const,
  authorizations: (callId: string) => [...recordingKeys.all, callId, "authorizations"] as const,
  recordings: (callId: string) => [...recordingKeys.all, callId, "list"] as const,
};

export function useRecordingAuthorizations(callId: string, enabled = true) {
  return useQuery({
    queryKey: recordingKeys.authorizations(callId),
    queryFn: () => recordingService.getAuthorizations(callId),
    enabled: !!callId && enabled,
    refetchInterval: 5000,
    retry: false,
  });
}

export function useCallRecordings(callId: string, enabled = true) {
  return useQuery({
    queryKey: recordingKeys.recordings(callId),
    queryFn: () => recordingService.listRecordings(callId),
    enabled: !!callId && enabled,
    refetchInterval: 5000,
    retry: false,
  });
}

function useRecordingMutation<T>(callId: string, mutationFn: (input: T) => Promise<unknown>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: [...recordingKeys.all, callId] });
      void queryClient.invalidateQueries({ queryKey: callingKeys.detail(callId) });
    },
  });
}

export function useRecordConsent(callId: string) {
  return useRecordingMutation(callId, (data: RecordConsentRequest) => recordingService.recordConsent(callId, data));
}

export function useStartRecording(callId: string) {
  return useRecordingMutation(callId, (key: string | undefined) => recordingService.startRecording(callId, key));
}

export function useStopRecording(callId: string) {
  return useRecordingMutation(callId, (recordingId: string) => recordingService.stopRecording(callId, recordingId));
}
