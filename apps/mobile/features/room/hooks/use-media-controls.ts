import { useMutation } from "@tanstack/react-query";
import { roomApi } from "../api/room.api";
import { getApiError } from "@/lib/api/client";

export function useUpdateAudioState(participantId: string) {
  return useMutation({
    mutationFn: async (enabled: boolean) => {
      try {
        return await roomApi.updateAudioState(participantId, { enabled });
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

export function useUpdateVideoState(participantId: string) {
  return useMutation({
    mutationFn: async (enabled: boolean) => {
      try {
        return await roomApi.updateVideoState(participantId, { enabled });
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}

export function useScreenShare(meetingId: string) {
  const startMutation = useMutation({
    mutationFn: async () => {
      try {
        return await roomApi.startScreenShare(meetingId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });

  const stopMutation = useMutation({
    mutationFn: async () => {
      try {
        return await roomApi.stopScreenShare(meetingId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });

  return {
    startScreenShare: startMutation.mutate,
    stopScreenShare: stopMutation.mutate,
    isStarting: startMutation.isPending,
    isStopping: stopMutation.isPending,
  };
}

export function useEndMeeting(meetingId: string) {
  return useMutation({
    mutationFn: async () => {
      try {
        return await roomApi.endMeeting(meetingId);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
  });
}
