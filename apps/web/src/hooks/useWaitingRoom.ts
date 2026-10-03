import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

export function useWaitingParticipants(meetingId: string, enabled = true) {
  return useQuery({
    queryKey: ["waitingParticipants", meetingId],
    queryFn: () => meetingService.getWaitingParticipants(meetingId),
    enabled: !!meetingId && enabled,
    refetchInterval: 5000, // Poll every 5 seconds for new waiting participants
  });
}

export function useAdmitParticipant() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      meetingId,
      participantId,
    }: {
      meetingId: string;
      participantId: string;
    }) => meetingService.admitParticipant(meetingId, participantId),
    onSuccess: (_, variables) => {
      queryClient.invalidateQueries({
        queryKey: ["waitingParticipants", variables.meetingId],
      });
      queryClient.invalidateQueries({
        queryKey: ["participants", variables.meetingId],
      });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Failed to admit participant";
      console.error("Admit participant error:", message);
    },
  });
}

export function useDenyParticipant() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      meetingId,
      participantId,
    }: {
      meetingId: string;
      participantId: string;
    }) => meetingService.denyParticipant(meetingId, participantId),
    onSuccess: (_, variables) => {
      queryClient.invalidateQueries({
        queryKey: ["waitingParticipants", variables.meetingId],
      });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Failed to deny participant";
      console.error("Deny participant error:", message);
    },
  });
}
