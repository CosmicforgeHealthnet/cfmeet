/**
 * Calendar hooks (Blueprint Section 10 / 17): one agenda query per visible range, occurrence
 * changes, calendar files and the meeting capabilities of the selected workspace.
 */
import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { conferencesService } from "@/services/v2/conferences.service";
import { spacesService } from "@/services/v2/spaces.service";
import { agendaItemKey, fetchAgendaRange } from "@/lib/meeting-calendar";
import { useWorkspace } from "@/context/WorkspaceContext";
import type { AgendaQuery } from "@/types/generated/meeting-service";
import type { RecurrenceExceptionRequest, RescheduleConferenceRequest } from "@/types/v2";

export const calendarKeys = {
  all: ["v2", "conferences", "agenda"] as const,
  range: (query: Omit<AgendaQuery, "cursor">) => [...calendarKeys.all, query] as const,
};

/** Every occurrence of `[from, to)` (cursor pages merged, bounded). */
export function useAgendaRange(query: Omit<AgendaQuery, "cursor">, enabled = true) {
  return useQuery({
    queryKey: calendarKeys.range(query),
    queryFn: () => fetchAgendaRange((page) => conferencesService.agenda(page), query),
    enabled,
    staleTime: 30_000,
    placeholderData: (previous) => previous,
    retry: (count, error) => count < 2 && ![401, 403].includes((error as { response?: { status?: number } })?.response?.status ?? 0),
  });
}

const minute = () => Math.floor(Date.now() / 60_000) * 60_000;

/**
 * What's next: meetings starting from an hour ago over the next week, plus any meeting still
 * LIVE that started earlier (the agenda filters by start time, so a long meeting would
 * otherwise drop off while people are still in it). The window moves on every 5 minutes.
 */
export function useUpcomingMeetings(limit: number, organizationId?: string | null) {
  const [now, setNow] = useState(minute);
  useEffect(() => {
    const t = setInterval(() => setNow(minute()), 5 * 60_000);
    return () => clearInterval(t);
  }, []);
  const org = organizationId ? { organizationId } : {};
  const upcoming = useAgendaPage(useMemo(() => ({
    from: new Date(now - 60 * 60_000).toISOString(),
    to: new Date(now + 7 * 86_400_000).toISOString(),
    status: "CREATED,PREPARING,WAITING,LIVE",
    limit,
    ...org,
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }), [now, limit, organizationId]));
  const live = useAgendaPage(useMemo(() => ({
    from: new Date(now - 24 * 3_600_000).toISOString(),
    to: new Date(now - 60 * 60_000).toISOString(),
    status: "LIVE",
    limit: 5,
    ...org,
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }), [now, organizationId]));
  const data = useMemo(() => {
    if (!upcoming.data) return undefined;
    const seen = new Set<string>();
    const items = [...(live.data?.items ?? []), ...upcoming.data.items].filter((i) => {
      const k = agendaItemKey(i);
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    });
    return { ...upcoming.data, items: items.slice(0, limit) };
  }, [upcoming.data, live.data, limit]);
  return { ...upcoming, data };
}

/** One page of the agenda (dashboard "upcoming"). */
export function useAgendaPage(query: AgendaQuery, enabled = true) {
  return useQuery({
    queryKey: [...calendarKeys.all, "page", query] as const,
    queryFn: () => conferencesService.agenda(query),
    enabled,
    staleTime: 30_000,
    retry: (count, error) => count < 2 && ![401, 403].includes((error as { response?: { status?: number } })?.response?.status ?? 0),
  });
}

function useInvalidateSchedules() {
  const queryClient = useQueryClient();
  return () => Promise.all([
    queryClient.invalidateQueries({ queryKey: ["v2", "conferences"] }),
    queryClient.invalidateQueries({ queryKey: ["v2", "spaces"] }),
  ]);
}

/** `POST /conferences/{id}/reschedule` for any occurrence. */
export function useRescheduleOccurrence() {
  const invalidate = useInvalidateSchedules();
  return useMutation({
    mutationFn: ({ id, data }: { id: string; data: RescheduleConferenceRequest }) => conferencesService.reschedule(id, data),
    onSettled: invalidate,
  });
}

/** `POST /conferences/{id}/cancel`. */
export function useCancelOccurrence() {
  const invalidate = useInvalidateSchedules();
  return useMutation({
    mutationFn: (id: string) => conferencesService.cancel(id),
    onSettled: invalidate,
  });
}

/** `PUT /meeting-spaces/{id}/recurrence/exceptions` (single occurrence of a series, materialized or not). */
export function useOccurrenceException() {
  const invalidate = useInvalidateSchedules();
  return useMutation({
    mutationFn: ({ spaceId, data }: { spaceId: string; data: RecurrenceExceptionRequest }) => spacesService.setException(spaceId, data),
    onSettled: invalidate,
  });
}

/** Saves a downloaded file (the `.ics` endpoints need the bearer token, so no plain link). */
export function saveFile(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

/** Download `GET /conferences/{id}/calendar.ics` or `GET /meeting-spaces/{id}/calendar.ics`. */
export function useCalendarDownload() {
  return useMutation({
    mutationFn: async ({ kind, id, name }: { kind: "conference" | "space"; id: string; name?: string }) => {
      const blob = kind === "conference" ? await conferencesService.calendarFile(id) : await spacesService.calendarFile(id);
      const safe = (name || (kind === "conference" ? "meeting-occurrence" : "meeting")).replace(/[^\w.-]+/g, "-").slice(0, 60) || "meeting";
      saveFile(blob, `${safe}.ics`);
    },
  });
}

/**
 * What the selected workspace lets the caller do with meetings. Organization meetings need
 * `meetings.organization.create` (default deny). Personal creation is denied only when the
 * server says so (`meetings.personal.create`); while capabilities load the backend decides.
 */
export function useMeetingCapabilities() {
  const workspace = useWorkspace();
  const personalDecision = workspace.decision("meetings.personal.create");
  const organization = workspace.kind === "ORGANIZATION" && !!workspace.organizationId;
  const featureOff = workspace.capabilities?.features.meetings === false;
  return {
    workspaceId: workspace.workspaceId,
    organizationId: organization ? workspace.organizationId : null,
    organizationName: workspace.workspace?.displayName ?? null,
    canCreatePersonal: !featureOff && (personalDecision.reason === "UNKNOWN" || personalDecision.granted),
    canCreateOrganization: organization && !featureOff && workspace.can("meetings.organization.create"),
    organizationCreateDecision: workspace.decision("meetings.organization.create"),
    loading: workspace.isLoading,
  };
}
