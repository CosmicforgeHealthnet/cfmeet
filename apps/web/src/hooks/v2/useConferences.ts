/**
 * V2 Conferences Hooks
 * React Query hooks for conference operations
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { conferencesService } from "@/services/v2";
import type {
  CreateConferenceRequest,
  RescheduleConferenceRequest,
  ListConferencesQuery,
  JoinConferenceRequestV2,
  LeaveConferenceRequest,
} from "@/types/v2";
import { spaceKeys } from "./useSpaces";

// Query Keys
export const conferenceKeys = {
  all: ["v2", "conferences"] as const,
  lists: () => [...conferenceKeys.all, "list"] as const,
  list: (spaceId: string, query?: ListConferencesQuery) =>
    [...conferenceKeys.lists(), spaceId, query] as const,
  details: () => [...conferenceKeys.all, "detail"] as const,
  detail: (id: string) => [...conferenceKeys.details(), id] as const,
  preJoin: (id: string) => [...conferenceKeys.detail(id), "pre-join"] as const,
};

// -------------------------------------------------------------------------
// Queries
// -------------------------------------------------------------------------

/**
 * List conferences for a meeting space
 */
export function useConferences(spaceId: string, query?: ListConferencesQuery) {
  return useQuery({
    queryKey: conferenceKeys.list(spaceId, query),
    queryFn: () => conferencesService.list(spaceId, query),
    enabled: !!spaceId,
    staleTime: 1000 * 30, // 30 seconds
  });
}

/**
 * Get a single conference
 */
export function useConference(id: string) {
  return useQuery({
    queryKey: conferenceKeys.detail(id),
    queryFn: () => conferencesService.get(id),
    enabled: !!id,
    refetchInterval: 5000, // Poll every 5 seconds for live updates
  });
}

/**
 * Get pre-join information
 */
export function usePreJoin(conferenceId: string) {
  return useQuery({
    queryKey: conferenceKeys.preJoin(conferenceId),
    queryFn: () => conferencesService.preJoin(conferenceId),
    enabled: !!conferenceId,
    staleTime: 1000 * 60, // 1 minute
  });
}

// -------------------------------------------------------------------------
// Mutations
// -------------------------------------------------------------------------

/**
 * Create a new conference
 */
export function useCreateConference(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data?: CreateConferenceRequest) =>
      conferencesService.create(spaceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: conferenceKeys.lists() });
      queryClient.invalidateQueries({ queryKey: spaceKeys.detail(spaceId) });
    },
  });
}

/**
 * Prepare a conference
 */
export function usePrepareConference() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => conferencesService.prepare(id),
    onSuccess: (data) => {
      queryClient.setQueryData(conferenceKeys.detail(data.conference.id), data.conference);
    },
  });
}

/**
 * Start a conference
 */
export function useStartConference() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => conferencesService.start(id),
    onSuccess: (data) => {
      queryClient.setQueryData(conferenceKeys.detail(data.conference.id), data.conference);
    },
  });
}

/**
 * Lock a conference
 */
export function useLockConference() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => conferencesService.lock(id),
    onSuccess: (data) => {
      queryClient.setQueryData(conferenceKeys.detail(data.conference.id), data.conference);
    },
  });
}

/**
 * Unlock a conference
 */
export function useUnlockConference() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => conferencesService.unlock(id),
    onSuccess: (data) => {
      queryClient.setQueryData(conferenceKeys.detail(data.conference.id), data.conference);
    },
  });
}

/**
 * End a conference
 */
export function useEndConference() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => conferencesService.end(id),
    onSuccess: (data) => {
      queryClient.setQueryData(conferenceKeys.detail(data.conference.id), data.conference);
      queryClient.invalidateQueries({ queryKey: conferenceKeys.lists() });
    },
  });
}

/**
 * Cancel a conference
 */
export function useCancelConference() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => conferencesService.cancel(id),
    onSuccess: (data) => {
      queryClient.setQueryData(conferenceKeys.detail(data.conference.id), data.conference);
      queryClient.invalidateQueries({ queryKey: conferenceKeys.lists() });
    },
  });
}

/**
 * Reschedule a conference
 */
export function useRescheduleConference(id: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: RescheduleConferenceRequest) =>
      conferencesService.reschedule(id, data),
    onSuccess: (data) => {
      queryClient.setQueryData(conferenceKeys.detail(id), data.conference);
      queryClient.invalidateQueries({ queryKey: conferenceKeys.lists() });
    },
  });
}

/**
 * Join a conference
 */
export function useJoinConference() {
  return useMutation({
    mutationFn: ({ id, data }: { id: string; data?: JoinConferenceRequestV2 }) =>
      conferencesService.join(id, data),
  });
}

/**
 * Leave a conference
 */
export function useLeaveConference() {
  return useMutation({
    mutationFn: ({ id, data }: { id: string; data?: LeaveConferenceRequest }) =>
      conferencesService.leave(id, data),
  });
}
