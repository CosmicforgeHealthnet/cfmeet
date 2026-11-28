"use client";

/** Events (ADR 0071) queries and changes; every list is refreshed after a change. */
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { eventsService, isNotFound } from "@/services/v2/events.service";
import { mergeEventPages, type CalendarEvent, type EventFilters, type RsvpResponse } from "@/lib/events/core";

export const eventKeys = {
  all: ["v2", "messaging", "events"] as const,
  list: (filters: Omit<EventFilters, "cursor">) => [...eventKeys.all, "list", filters] as const,
  detail: (eventId: string) => [...eventKeys.all, "detail", eventId] as const,
  attendees: (eventId: string) => [...eventKeys.all, "attendees", eventId] as const,
};

const retry = (count: number, error: unknown) => count < 2 && !isNotFound(error) && ![401, 403, 422].includes((error as { status?: number })?.status ?? 0);

/** One list view, "Load more" through `nextCursor`. */
export function useEventList(filters: Omit<EventFilters, "cursor">, enabled = true) {
  const query = useInfiniteQuery({
    queryKey: eventKeys.list(filters),
    queryFn: ({ pageParam }) => eventsService.list({ ...filters, cursor: pageParam }),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    enabled,
    staleTime: 30_000,
    retry,
  });
  return { ...query, items: mergeEventPages(query.data?.pages) };
}

/** Every occurrence of a calendar window (cursor pages followed, bounded). */
export function useEventRange(filters: Omit<EventFilters, "cursor">, enabled = true) {
  return useQuery({
    queryKey: [...eventKeys.list(filters), "all"],
    queryFn: async () => {
      const pages = [];
      let cursor: string | null = null;
      for (let i = 0; i < 10; i++) {
        const page = await eventsService.list({ ...filters, limit: 100, cursor });
        pages.push(page);
        cursor = page.nextCursor;
        if (!cursor) break;
      }
      return mergeEventPages(pages);
    },
    enabled,
    staleTime: 30_000,
    placeholderData: (previous) => previous,
    retry,
  });
}

export function useEvent(eventId: string | null | undefined) {
  return useQuery({
    queryKey: eventKeys.detail(eventId ?? ""),
    queryFn: () => eventsService.get(eventId!),
    enabled: !!eventId,
    staleTime: 15_000,
    retry,
  });
}

export function useEventAttendees(eventId: string | null | undefined, enabled = true) {
  return useQuery({
    queryKey: eventKeys.attendees(eventId ?? ""),
    queryFn: () => eventsService.attendees(eventId!),
    enabled: !!eventId && enabled,
    staleTime: 15_000,
    retry,
  });
}

/** After any change: the event itself is the answer; lists and attendees are refetched. */
export function useEventChanged() {
  const client = useQueryClient();
  return (event?: CalendarEvent | null, eventId?: string) => {
    const id = event?.eventId ?? eventId;
    if (event) client.setQueryData(eventKeys.detail(event.eventId), (old: CalendarEvent | undefined) => ({ ...old, ...event, upcomingOccurrences: event.upcomingOccurrences ?? old?.upcomingOccurrences }));
    void client.invalidateQueries({ queryKey: [...eventKeys.all, "list"] });
    if (id) void client.invalidateQueries({ queryKey: eventKeys.attendees(id) });
  };
}

export function useRespond(eventId: string) {
  const changed = useEventChanged();
  return useMutation({
    mutationFn: (response: RsvpResponse) => eventsService.respond(eventId, response),
    onSuccess: (event) => changed(event),
  });
}
