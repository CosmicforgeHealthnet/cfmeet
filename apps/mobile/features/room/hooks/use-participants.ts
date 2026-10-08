import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { QUERY_KEYS } from "@/lib/constants";
import { roomApi } from "../api/room.api";
import { getApiError } from "@/lib/api/client";

export function useParticipants(meetingId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.PARTICIPANTS(meetingId),
    queryFn: () => roomApi.getParticipants(meetingId),
    enabled: !!meetingId,
    refetchInterval: 5000,
  });
}

export function useWaitingParticipants(meetingId: string, isHost: boolean) {
  return useQuery({
    queryKey: QUERY_KEYS.WAITING_PARTICIPANTS(meetingId),
    queryFn: () => roomApi.getWaitingParticipants(meetingId),
    enabled: !!meetingId && isHost,
    refetchInterval: 3000,
  });
}

export function useAdmitParticipant(meetingId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantId: string) => {
      try {
        return await roomApi.admitParticipant(meetingId, participantId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: QUERY_KEYS.WAITING_PARTICIPANTS(meetingId),
      });
      queryClient.invalidateQueries({
        queryKey: QUERY_KEYS.PARTICIPANTS(meetingId),
      });
    },
  });
}

export function useDenyParticipant(meetingId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantId: string) => {
      try {
        return await roomApi.denyParticipant(meetingId, participantId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: QUERY_KEYS.WAITING_PARTICIPANTS(meetingId),
      });
    },
  });
}

export function useAdmitAllParticipants(meetingId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (participantIds: string[]) => {
      try {
        await Promise.all(
          participantIds.map((id) => roomApi.admitParticipant(meetingId, id))
        );
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: QUERY_KEYS.WAITING_PARTICIPANTS(meetingId),
      });
      queryClient.invalidateQueries({
        queryKey: QUERY_KEYS.PARTICIPANTS(meetingId),
      });
    },
  });
}
