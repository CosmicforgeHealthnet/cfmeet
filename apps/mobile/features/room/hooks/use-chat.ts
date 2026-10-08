import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { QUERY_KEYS } from "@/lib/constants";
import { roomApi } from "../api/room.api";
import { getApiError } from "@/lib/api/client";

export function useChat(meetingId: string) {
  return useQuery({
    queryKey: QUERY_KEYS.CHAT(meetingId),
    queryFn: () => roomApi.getChatMessages(meetingId),
    enabled: !!meetingId,
    refetchInterval: 3000,
  });
}

export function useSendMessage(meetingId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (message: string) => {
      try {
        return await roomApi.sendChatMessage(meetingId, { message });
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: QUERY_KEYS.CHAT(meetingId),
      });
    },
  });
}
