/**
 * V2 Collaboration Hooks
 * React Query hooks for media controls, chat, reactions, and hand raise
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { collaborationService } from "@/services/v2";
import type {
  CollaborationPolicyUpdateRequest,
  MediaCredentialRequest,
  SpotlightRequest,
  MessageListQuery,
  SendMessageRequest,
  ChatPolicyUpdateRequest,
  ReactionRequest,
} from "@/types/v2";

// Query Keys
export const collaborationKeys = {
  all: ["v2", "collaboration"] as const,
  state: (conferenceId: string) => [...collaborationKeys.all, "state", conferenceId] as const,
  messages: (conferenceId: string, query?: MessageListQuery) =>
    [...collaborationKeys.all, "messages", conferenceId, query] as const,
  raisedHands: (conferenceId: string) =>
    [...collaborationKeys.all, "raised-hands", conferenceId] as const,
};

// -------------------------------------------------------------------------
// Queries
// -------------------------------------------------------------------------

/**
 * Get collaboration state
 */
export function useCollaborationState(conferenceId: string) {
  return useQuery({
    queryKey: collaborationKeys.state(conferenceId),
    queryFn: () => collaborationService.getState(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 3000,
  });
}

/**
 * Get chat messages
 */
export function useMessages(conferenceId: string, query?: MessageListQuery) {
  return useQuery({
    queryKey: collaborationKeys.messages(conferenceId, query),
    queryFn: () => collaborationService.getMessages(conferenceId, query),
    enabled: !!conferenceId,
    refetchInterval: 2000,
  });
}

/**
 * Get raised hands
 */
export function useRaisedHands(conferenceId: string) {
  return useQuery({
    queryKey: collaborationKeys.raisedHands(conferenceId),
    queryFn: () => collaborationService.getRaisedHands(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 2000,
  });
}

// -------------------------------------------------------------------------
// Mutations - Policy & Credentials
// -------------------------------------------------------------------------

/**
 * Update collaboration policy
 */
export function useUpdateCollaborationPolicy(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: CollaborationPolicyUpdateRequest) =>
      collaborationService.updatePolicy(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Get/refresh media credential
 */
export function useMediaCredential(conferenceId: string) {
  return useMutation({
    mutationFn: (data?: MediaCredentialRequest) =>
      collaborationService.getMediaCredential(conferenceId, data),
  });
}

// -------------------------------------------------------------------------
// Mutations - Media Locks
// -------------------------------------------------------------------------

/**
 * Lock audio for a participant
 */
export function useLockAudio(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.lockAudio(conferenceId, participantId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Unlock audio for a participant
 */
export function useUnlockAudio(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.unlockAudio(conferenceId, participantId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Lock video for a participant
 */
export function useLockVideo(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.lockVideo(conferenceId, participantId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Unlock video for a participant
 */
export function useUnlockVideo(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.unlockVideo(conferenceId, participantId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Stop screen share for a participant
 */
export function useStopScreenShare(conferenceId: string) {
  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.stopScreenShare(conferenceId, participantId),
  });
}

// -------------------------------------------------------------------------
// Mutations - Spotlights
// -------------------------------------------------------------------------

/**
 * Add a participant to spotlight
 */
export function useAddSpotlight(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: SpotlightRequest) =>
      collaborationService.addSpotlight(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Remove a participant from spotlight
 */
export function useRemoveSpotlight(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.removeSpotlight(conferenceId, participantId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

// -------------------------------------------------------------------------
// Mutations - Chat
// -------------------------------------------------------------------------

/**
 * Send a chat message
 */
export function useSendMessage(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: SendMessageRequest) =>
      collaborationService.sendMessage(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.messages(conferenceId),
      });
    },
  });
}

/**
 * Delete a chat message
 */
export function useDeleteMessage(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (messageId: string) =>
      collaborationService.deleteMessage(conferenceId, messageId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.messages(conferenceId),
      });
    },
  });
}

/**
 * Update chat policy
 */
export function useUpdateChatPolicy(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: ChatPolicyUpdateRequest) =>
      collaborationService.updateChatPolicy(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Restrict chat for a participant
 */
export function useRestrictChat(conferenceId: string) {
  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.restrictChat(conferenceId, participantId),
  });
}

/**
 * Unrestrict chat for a participant
 */
export function useUnrestrictChat(conferenceId: string) {
  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.unrestrictChat(conferenceId, participantId),
  });
}

// -------------------------------------------------------------------------
// Mutations - Reactions & Hand Raise
// -------------------------------------------------------------------------

/**
 * Send a reaction
 */
export function useSendReaction(conferenceId: string) {
  return useMutation({
    mutationFn: (data: ReactionRequest) =>
      collaborationService.sendReaction(conferenceId, data),
  });
}

/**
 * Raise hand
 */
export function useRaiseHand(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: () => collaborationService.raiseHand(conferenceId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.raisedHands(conferenceId),
      });
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Lower hand (self)
 */
export function useLowerHand(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: () => collaborationService.lowerHand(conferenceId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.raisedHands(conferenceId),
      });
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.state(conferenceId),
      });
    },
  });
}

/**
 * Lower someone else's hand
 */
export function useLowerOtherHand(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (participantId: string) =>
      collaborationService.lowerOtherHand(conferenceId, participantId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: collaborationKeys.raisedHands(conferenceId),
      });
    },
  });
}
