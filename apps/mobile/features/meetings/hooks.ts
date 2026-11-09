/** React Query hooks for meetings, spaces and the agenda (Meeting Service v2 only). */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { meetingsApi, idempotencyKey } from "@/features/meetings/api";
import { itemsForWorkspace } from "@/lib/meeting/agenda";
import { toIsoUtc } from "@/lib/time/zoned";
import type {
  CreateMeetingSpaceRequest,
  InstantMeetingRequest,
  RescheduleConferenceRequest,
  UpdateMeetingSpaceRequest,
} from "@/lib/api/generated/meeting-service";

export const meetingKeys = {
  agenda: (workspaceId: string, from: string, to: string) => ["ctx", workspaceId, "agenda", from, to] as const,
  spaces: (workspaceId: string) => ["ctx", workspaceId, "spaces"] as const,
  space: (id: string) => ["meeting-space", id] as const,
  conferences: (id: string) => ["meeting-space", id, "conferences"] as const,
};

/** Agenda of the selected workspace between two instants. */
export function useAgenda(workspaceId: string, organizationId: string | undefined, fromMs: number, toMs: number) {
  const from = toIsoUtc(fromMs);
  const to = toIsoUtc(toMs);
  return useQuery({
    queryKey: meetingKeys.agenda(workspaceId, from, to),
    queryFn: async () => {
      const items = [];
      let cursor: string | undefined;
      for (let page = 0; page < 5; page++) {
        const res = await meetingsApi.agenda({ from, to, organizationId, cursor });
        items.push(...res.items);
        if (!res.nextCursor) break;
        cursor = res.nextCursor;
      }
      return itemsForWorkspace(items, organizationId);
    },
    refetchInterval: 60_000,
  });
}

/** Meeting spaces the user can manage in the selected workspace. */
export function useSpaces(workspaceId: string, organizationId: string | undefined) {
  return useQuery({
    queryKey: meetingKeys.spaces(workspaceId),
    queryFn: async () => {
      const items = [];
      let cursor: string | undefined;
      for (let page = 0; page < 5; page++) {
        const res = await meetingsApi.listSpaces(cursor);
        items.push(...res.items);
        if (!res.nextCursor) break;
        cursor = res.nextCursor;
      }
      return items.filter(
        (s) => s.status === "ACTIVE" && (organizationId ? s.organizationId === organizationId : !s.organizationId)
      );
    },
  });
}

export function useSpace(id: string) {
  return useQuery({ queryKey: meetingKeys.space(id), queryFn: () => meetingsApi.getSpace(id), enabled: !!id });
}

export function useSpaceConferences(id: string) {
  return useQuery({
    queryKey: meetingKeys.conferences(id),
    queryFn: () => meetingsApi.listConferences(id),
    enabled: !!id,
  });
}

function useInvalidateMeetings() {
  const qc = useQueryClient();
  return (spaceId?: string) => {
    void qc.invalidateQueries({ predicate: (q) => q.queryKey[0] === "ctx" && (q.queryKey[2] === "agenda" || q.queryKey[2] === "spaces") });
    if (spaceId) {
      void qc.invalidateQueries({ queryKey: meetingKeys.space(spaceId) });
      void qc.invalidateQueries({ queryKey: meetingKeys.conferences(spaceId) });
    }
  };
}

export interface ScheduleInput {
  space: CreateMeetingSpaceRequest;
  startMs: number;
  endMs: number | null;
}

/** Schedule = create the meeting space, then its first conference (both idempotent). */
export function useScheduleMeeting() {
  const invalidate = useInvalidateMeetings();
  return useMutation({
    mutationFn: async ({ space, startMs, endMs }: ScheduleInput) => {
      const created = await meetingsApi.createSpace(space, idempotencyKey());
      const conference = await meetingsApi.createConference(
        created.id,
        { scheduledStartAt: toIsoUtc(startMs), scheduledEndAt: endMs !== null ? toIsoUtc(endMs) : null },
        idempotencyKey()
      );
      return { space: created, conference };
    },
    onSuccess: ({ space }) => invalidate(space.id),
  });
}

export function useInstantMeeting() {
  const invalidate = useInvalidateMeetings();
  return useMutation({
    mutationFn: (data: InstantMeetingRequest) => meetingsApi.instantMeeting(data),
    onSuccess: (res) => invalidate(res.meetingSpace.id),
  });
}

export function useAddConference(spaceId: string) {
  const invalidate = useInvalidateMeetings();
  return useMutation({
    mutationFn: ({ startMs, endMs }: { startMs: number; endMs: number | null }) =>
      meetingsApi.createConference(
        spaceId,
        { scheduledStartAt: toIsoUtc(startMs), scheduledEndAt: endMs !== null ? toIsoUtc(endMs) : null },
        idempotencyKey()
      ),
    onSuccess: () => invalidate(spaceId),
  });
}

export function useReschedule(spaceId: string) {
  const invalidate = useInvalidateMeetings();
  return useMutation({
    mutationFn: ({ conferenceId, data }: { conferenceId: string; data: RescheduleConferenceRequest }) =>
      meetingsApi.rescheduleConference(conferenceId, data),
    onSuccess: () => invalidate(spaceId),
  });
}

export function useCancelConference(spaceId: string) {
  const invalidate = useInvalidateMeetings();
  return useMutation({
    mutationFn: (conferenceId: string) => meetingsApi.cancelConference(conferenceId),
    onSuccess: () => invalidate(spaceId),
  });
}

export function useUpdateSpace(spaceId: string) {
  const invalidate = useInvalidateMeetings();
  return useMutation({
    mutationFn: (data: UpdateMeetingSpaceRequest) => meetingsApi.updateSpace(spaceId, data),
    onSuccess: () => invalidate(spaceId),
  });
}

export function useArchiveSpace(spaceId: string) {
  const invalidate = useInvalidateMeetings();
  return useMutation({
    mutationFn: (version: number | null | undefined) => meetingsApi.archiveSpace(spaceId, { version: version ?? null }),
    onSuccess: () => invalidate(spaceId),
  });
}
