/**
 * V2 Breakouts Hooks
 * React Query hooks for breakout room management
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { breakoutsService } from "@/services/v2";
import type {
  CreateBreakoutsRequest,
  AssignBreakoutsRequest,
  MoveParticipantRequest,
  RoomAssignmentsPatch,
  BreakoutAnnouncementRequest,
  CloseBreakoutRequest,
} from "@/types/v2";

// Query Keys
export const breakoutKeys = {
  all: ["v2", "breakouts"] as const,
  list: (conferenceId: string) => [...breakoutKeys.all, "list", conferenceId] as const,
};

// -------------------------------------------------------------------------
// Queries
// -------------------------------------------------------------------------

/**
 * List breakout rooms
 */
export function useBreakouts(conferenceId: string) {
  return useQuery({
    queryKey: breakoutKeys.list(conferenceId),
    queryFn: () => breakoutsService.list(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 3000,
  });
}

// -------------------------------------------------------------------------
// Mutations
// -------------------------------------------------------------------------

/**
 * Create breakout rooms
 */
export function useCreateBreakouts(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: CreateBreakoutsRequest) =>
      breakoutsService.create(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}

/**
 * Assign participants to breakout rooms
 */
export function useAssignBreakouts(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: AssignBreakoutsRequest) =>
      breakoutsService.assign(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}

/**
 * Move a participant between rooms
 */
export function useMoveParticipant(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: MoveParticipantRequest) =>
      breakoutsService.moveParticipant(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}

/**
 * Leave current breakout
 */
export function useLeaveBreakout(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: () => breakoutsService.leave(conferenceId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}

/**
 * Send announcement to all breakout rooms
 */
export function useAnnounceAll(conferenceId: string) {
  return useMutation({
    mutationFn: (data: BreakoutAnnouncementRequest) =>
      breakoutsService.announceAll(conferenceId, data),
  });
}

/**
 * Close all breakout rooms
 */
export function useCloseAllBreakouts(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data?: CloseBreakoutRequest) =>
      breakoutsService.closeAll(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}

/**
 * Patch assignments for a single breakout room
 */
export function usePatchAssignments(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      breakoutId,
      data,
    }: {
      breakoutId: string;
      data: RoomAssignmentsPatch;
    }) => breakoutsService.patchAssignments(conferenceId, breakoutId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}

/**
 * Open a breakout room
 */
export function useOpenBreakout(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (breakoutId: string) =>
      breakoutsService.open(conferenceId, breakoutId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}

/**
 * Enter a breakout room
 */
export function useEnterBreakout(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (breakoutId: string) =>
      breakoutsService.enter(conferenceId, breakoutId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}

/**
 * Send announcement to a single breakout room
 */
export function useAnnounceBreakout(conferenceId: string) {
  return useMutation({
    mutationFn: ({
      breakoutId,
      data,
    }: {
      breakoutId: string;
      data: BreakoutAnnouncementRequest;
    }) => breakoutsService.announce(conferenceId, breakoutId, data),
  });
}

/**
 * Close a single breakout room
 */
export function useCloseBreakout(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      breakoutId,
      data,
    }: {
      breakoutId: string;
      data?: CloseBreakoutRequest;
    }) => breakoutsService.close(conferenceId, breakoutId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    },
  });
}
