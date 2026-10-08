import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { QUERY_KEYS } from "@/lib/constants";
import { meetingsApi } from "../api/meetings.api";
import { getApiError } from "@/lib/api/client";
import type { CreateMeetingRequest, UpdateMeetingRequest } from "../types";

export function useMeetings(page = 1, limit = 10) {
  return useQuery({
    queryKey: [...QUERY_KEYS.MEETINGS, page, limit],
    queryFn: () => meetingsApi.getMeetings(page, limit),
    refetchInterval: 5000,
  });
}

export function useMeeting(id: string) {
  return useQuery({
    queryKey: QUERY_KEYS.MEETING(id),
    queryFn: () => meetingsApi.getMeeting(id),
    enabled: !!id,
  });
}

export function usePublicMeeting(identifier: string) {
  return useQuery({
    queryKey: QUERY_KEYS.PUBLIC_MEETING(identifier),
    queryFn: () => meetingsApi.getPublicMeeting(identifier),
    enabled: !!identifier,
    retry: false,
  });
}

export function useCreateMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (data: CreateMeetingRequest) => {
      try {
        return await meetingsApi.createMeeting(data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.MEETINGS });
    },
  });
}

export function useUpdateMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ id, data }: { id: string; data: UpdateMeetingRequest }) => {
      try {
        return await meetingsApi.updateMeeting(id, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: (_, variables) => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.MEETINGS });
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.MEETING(variables.id) });
    },
  });
}

export function useDeleteMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (id: string) => {
      try {
        return await meetingsApi.deleteMeeting(id);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.MEETINGS });
    },
  });
}

export function useInstantMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      try {
        return await meetingsApi.createInstantMeeting();
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.MEETINGS });
    },
  });
}

export function useEndMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (id: string) => {
      try {
        return await meetingsApi.endMeeting(id);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: QUERY_KEYS.MEETINGS });
    },
  });
}
