/**
 * Meeting code / link → the conference to join (`GET /api/v2/resolve/{identifier}`).
 * Works signed in or as a guest; routes to the pre-join screen of the resolved conference.
 */
import { useState, useCallback } from "react";
import { router } from "expo-router";
import { meetingsApi, idempotencyKey } from "@/features/meetings/api";
import { decodeApiError } from "@/lib/api/errors";
import { parseMeetingIdentifier } from "@/lib/invitations";
import { useAuthStore } from "@/store/auth.store";
import type { MeetingSpaceResponse } from "@/lib/api/generated/meeting-service";

const HOST_ROLES = ["OWNER", "HOST", "CO_HOST"];

export function canHost(space: Pick<MeetingSpaceResponse, "viewerRole">): boolean {
  return !!space.viewerRole && HOST_ROLES.includes(space.viewerRole);
}

export function goToPreJoin(conferenceId: string, space: Pick<MeetingSpaceResponse, "title" | "meetingCode">, invitation?: string) {
  router.push({
    pathname: "/(room)/[id]",
    params: {
      id: conferenceId,
      title: space.title ?? "",
      code: space.meetingCode,
      ...(invitation ? { invitation } : {}),
    },
  });
}

/** Opens a meeting space for joining: its current conference, or (hosts) a new one now. */
export async function openSpace(space: MeetingSpaceResponse, conferenceId: string | null | undefined, invitation?: string) {
  if (conferenceId) {
    goToPreJoin(conferenceId, space, invitation);
    return;
  }
  if (!canHost(space)) throw new Error("This meeting has no session right now. Ask the host to start it.");
  const conference = await meetingsApi.createConference(space.id, {}, idempotencyKey());
  goToPreJoin(conference.id, space);
}

export function useOpenMeeting() {
  const { isAuthenticated } = useAuthStore();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const open = useCallback(
    async (input: string, invitation?: string) => {
      setError(null);
      const identifier = parseMeetingIdentifier(input);
      if (!identifier) {
        setError("Enter a valid meeting code or link.");
        return;
      }
      setPending(true);
      try {
        const resolved = await meetingsApi.resolve(identifier, isAuthenticated);
        const current = resolved.conference;
        const live =
          current && !["ENDED", "CANCELLED", "FAILED"].includes(current.status)
            ? current.id
            : resolved.meetingSpace.latestConference &&
                !["ENDED", "CANCELLED", "FAILED"].includes(resolved.meetingSpace.latestConference.status)
              ? resolved.meetingSpace.latestConference.id
              : null;
        await openSpace(resolved.meetingSpace, live, invitation);
      } catch (e) {
        const decoded = decodeApiError(e);
        setError(decoded.kind === "not_found" ? "No meeting matches that code." : decoded.message);
      } finally {
        setPending(false);
      }
    },
    [isAuthenticated]
  );

  return { open, pending, error };
}
