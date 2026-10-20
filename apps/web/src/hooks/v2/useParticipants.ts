/**
 * V2 Participants Hooks
 * React Query hooks for participant management and moderation
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { participantsService } from "@/services/v2";
import type {
  AdmitRequest,
  DenyRequest,
  RoleChangeRequest,
  RemoveParticipantRequest,
  BanParticipantRequest,
  MuteRequest,
} from "@/types/v2";

// Query Keys
export const participantKeys = {
  all: ["v2", "participants"] as const,
  lists: () => [...participantKeys.all, "list"] as const,
  list: (conferenceId: string) => [...participantKeys.lists(), conferenceId] as const,
  me: (conferenceId: string) => [...participantKeys.all, "me", conferenceId] as const,
  waitingRoom: (conferenceId: string) =>
    [...participantKeys.all, "waiting-room", conferenceId] as const,
};

// -------------------------------------------------------------------------
// Queries
// -------------------------------------------------------------------------

/**
 * Get participant roster
 */
export function useParticipantsV2(conferenceId: string) {
  return useQuery({
    queryKey: participantKeys.list(conferenceId),
    queryFn: () => participantsService.list(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 3000, // Poll every 3 seconds
  });
}

/**
 * Get current participant (self)
 */
export function useCurrentParticipant(conferenceId: string) {
  return useQuery({
    queryKey: participantKeys.me(conferenceId),
    queryFn: () => participantsService.me(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 2000, // Poll every 2 seconds for admission updates
  });
}

/**
 * Get waiting room participants
 * @param conferenceId The conference ID
 * @param isHost Optional - only fetch if user is host (defaults to true)
 */
export function useWaitingRoomV2(conferenceId: string, isHost: boolean = true) {
  return useQuery({
    queryKey: participantKeys.waitingRoom(conferenceId),
    queryFn: () => participantsService.waitingRoom(conferenceId),
    enabled: !!conferenceId && isHost,
    refetchInterval: 2000, // Poll every 2 seconds
  });
}

// -------------------------------------------------------------------------
// Mutations
// -------------------------------------------------------------------------

/**
 * Admit participants from waiting room
 */
export function useAdmitParticipants(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: AdmitRequest) => participantsService.admit(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: participantKeys.waitingRoom(conferenceId),
      });
      queryClient.invalidateQueries({
        queryKey: participantKeys.list(conferenceId),
      });
    },
  });
}

/**
 * Deny participants from waiting room
 */
export function useDenyParticipants(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: DenyRequest) => participantsService.deny(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: participantKeys.waitingRoom(conferenceId),
      });
    },
  });
}

/**
 * Return participant to waiting room
 */
export function useReturnToWaiting(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (participantId: string) =>
      participantsService.returnToWaiting(conferenceId, participantId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: participantKeys.waitingRoom(conferenceId),
      });
      queryClient.invalidateQueries({
        queryKey: participantKeys.list(conferenceId),
      });
    },
  });
}

/**
 * Change participant role
 */
export function useChangeRole(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      participantId,
      data,
    }: {
      participantId: string;
      data: RoleChangeRequest;
    }) => participantsService.changeRole(conferenceId, participantId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: participantKeys.list(conferenceId),
      });
    },
  });
}

/**
 * Remove participant from conference
 */
export function useRemoveParticipant(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      participantId,
      data,
    }: {
      participantId: string;
      data?: RemoveParticipantRequest;
    }) => participantsService.remove(conferenceId, participantId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: participantKeys.list(conferenceId),
      });
    },
  });
}

/**
 * Ban participant
 */
export function useBanParticipant(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({
      participantId,
      data,
    }: {
      participantId: string;
      data?: BanParticipantRequest;
    }) => participantsService.ban(conferenceId, participantId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: participantKeys.list(conferenceId),
      });
    },
  });
}

/**
 * Server-side mute a participant
 */
export function useMuteParticipant(conferenceId: string) {
  return useMutation({
    mutationFn: ({
      participantId,
      data,
    }: {
      participantId: string;
      data: MuteRequest;
    }) => participantsService.mute(conferenceId, participantId, data),
  });
}
