/**
 * V2 Meeting Spaces Hooks
 * React Query hooks for meeting space operations
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { spacesService } from "@/services/v2";
import type {
  CreateMeetingSpaceRequest,
  UpdateMeetingSpaceRequest,
  DuplicateMeetingSpaceRequest,
  OwnerTransferRequest,
  InstantMeetingRequestV2,
  SetRecurrenceRequest,
  RecurrenceExceptionRequest,
} from "@/types/v2";

// Query Keys
export const spaceKeys = {
  all: ["v2", "spaces"] as const,
  lists: () => [...spaceKeys.all, "list"] as const,
  list: () => [...spaceKeys.lists()] as const,
  details: () => [...spaceKeys.all, "detail"] as const,
  detail: (id: string) => [...spaceKeys.details(), id] as const,
  resolve: (identifier: string) => [...spaceKeys.all, "resolve", identifier] as const,
  recurrence: (id: string) => [...spaceKeys.detail(id), "recurrence"] as const,
  exceptions: (id: string) => [...spaceKeys.detail(id), "exceptions"] as const,
};

// -------------------------------------------------------------------------
// Queries
// -------------------------------------------------------------------------

/**
 * List all meeting spaces
 */
export function useSpaces() {
  return useQuery({
    queryKey: spaceKeys.list(),
    queryFn: () => spacesService.list(),
    staleTime: 1000 * 60 * 5, // 5 minutes
  });
}

/**
 * Get a single meeting space by ID
 */
export function useSpace(id: string) {
  return useQuery({
    queryKey: spaceKeys.detail(id),
    queryFn: () => spacesService.get(id),
    enabled: !!id,
  });
}

/**
 * Resolve any meeting identifier
 */
export function useResolveSpace(identifier: string) {
  return useQuery({
    queryKey: spaceKeys.resolve(identifier),
    queryFn: () => spacesService.resolve(identifier),
    enabled: !!identifier,
    retry: false,
  });
}

/**
 * Get recurrence rule for a meeting space
 */
export function useSpaceRecurrence(spaceId: string) {
  return useQuery({
    queryKey: spaceKeys.recurrence(spaceId),
    queryFn: () => spacesService.getRecurrence(spaceId),
    enabled: !!spaceId,
  });
}

/**
 * Get recurrence exceptions for a meeting space
 */
export function useSpaceExceptions(spaceId: string) {
  return useQuery({
    queryKey: spaceKeys.exceptions(spaceId),
    queryFn: () => spacesService.getExceptions(spaceId),
    enabled: !!spaceId,
  });
}

// -------------------------------------------------------------------------
// Mutations
// -------------------------------------------------------------------------

/**
 * Create a new meeting space
 */
export function useCreateSpace() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: CreateMeetingSpaceRequest) => spacesService.create(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: spaceKeys.lists() });
    },
  });
}

/**
 * Update a meeting space
 */
export function useUpdateSpace(id: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: UpdateMeetingSpaceRequest) => spacesService.update(id, data),
    onSuccess: (data) => {
      queryClient.setQueryData(spaceKeys.detail(id), data);
      queryClient.invalidateQueries({ queryKey: spaceKeys.lists() });
    },
  });
}

/**
 * Archive a meeting space
 */
export function useArchiveSpace() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({ id, version }: { id: string; version?: number }) =>
      spacesService.archive(id, version),
    onSuccess: (data, { id }) => {
      queryClient.setQueryData(spaceKeys.detail(id), data);
      queryClient.invalidateQueries({ queryKey: spaceKeys.lists() });
    },
  });
}

/**
 * Duplicate a meeting space
 */
export function useDuplicateSpace() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({ id, data }: { id: string; data?: DuplicateMeetingSpaceRequest }) =>
      spacesService.duplicate(id, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: spaceKeys.lists() });
    },
  });
}

/**
 * Transfer ownership of a meeting space
 */
export function useTransferOwnership(id: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: OwnerTransferRequest) =>
      spacesService.transferOwnership(id, data),
    onSuccess: (data) => {
      queryClient.setQueryData(spaceKeys.detail(id), data);
      queryClient.invalidateQueries({ queryKey: spaceKeys.lists() });
    },
  });
}

/**
 * Create an instant meeting
 */
export function useCreateInstantMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data?: InstantMeetingRequestV2) => spacesService.createInstant(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: spaceKeys.lists() });
    },
  });
}

/**
 * Set or update recurrence rule
 */
export function useSetRecurrence(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: SetRecurrenceRequest) =>
      spacesService.setRecurrence(spaceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: spaceKeys.recurrence(spaceId) });
    },
  });
}

/**
 * Delete recurrence rule
 */
export function useDeleteRecurrence(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: () => spacesService.deleteRecurrence(spaceId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: spaceKeys.recurrence(spaceId) });
    },
  });
}

/**
 * Create or update a recurrence exception
 */
export function useSetException(spaceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: RecurrenceExceptionRequest) =>
      spacesService.setException(spaceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: spaceKeys.exceptions(spaceId) });
    },
  });
}
