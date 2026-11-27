"use client";

import { useCallback, useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useRealtime } from "@/hooks/v2/useRealtime";
import { conferenceKeys } from "@/hooks/v2/useConferences";
import { participantKeys } from "@/hooks/v2/useParticipants";
import { collaborationKeys } from "@/hooks/v2/useCollaboration";
import { breakoutKeys } from "@/hooks/v2/useBreakouts";
import { engagementKeys } from "@/hooks/v2/useEngagement";
import type { ConferenceResponse, RealtimeEnvelope } from "@/types/v2";
import { CAPTION_EVENT, MEDIA_MOVE_EVENT, emitNotice } from "./RoomLiveEvents";
import { markRealtime } from "@/lib/realtime-health";

export default function RealtimeRoomSync({ conferenceId }: { conferenceId: string }) {
  const queryClient = useQueryClient();

  const handleEvent = useCallback((event: RealtimeEnvelope) => {
    const type = event.eventType;
    if (type === "reaction.sent") {
      window.dispatchEvent(new CustomEvent("cfmeet:reaction", { detail: { id: event.eventId, reaction: event.payload.reaction, conferenceId } }));
      return;
    }
    const payload = (event.payload ?? {}) as Record<string, unknown>;
    if (type === "caption.segment") {
      window.dispatchEvent(new CustomEvent(CAPTION_EVENT, { detail: payload }));
      return;
    }
    if (type === "breakout.announcement") {
      emitNotice({ id: event.eventId, kind: "announcement", title: `Message from ${String(payload.fromDisplayName || "the host")}`, body: String(payload.message ?? "") });
    } else if (type === "breakout.closing") {
      emitNotice({ id: event.eventId, kind: "breakout", title: "Breakout rooms are closing", body: "You'll be back in the main room shortly." });
    } else if (type === "breakout.opened") {
      emitNotice({ id: event.eventId, kind: "breakout", title: "Breakout rooms are open", panel: "breakouts", action: "See rooms" });
    } else if (type === "conference.ending_soon") {
      const minutes = Number(payload.minutesRemaining ?? 5);
      const at = typeof payload.autoEndAt === "string" ? new Date(payload.autoEndAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }) : null;
      emitNotice({ id: "conference-ending", kind: "ending", title: `This meeting ends in ${minutes} minute${minutes === 1 ? "" : "s"}`, body: at ? `It will end automatically at ${at}.` : undefined, extendable: true });
    } else if (type === "conference.extended") {
      const at = typeof payload.scheduledEndAt === "string" ? new Date(payload.scheduledEndAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }) : null;
      emitNotice({ id: "conference-ending", kind: "ending", title: "The meeting was extended", body: at ? `It is now scheduled to end at ${at}.` : undefined });
    } else if (type === "conference.waiting_room_unattended") {
      emitNotice({ id: "waiting-room", kind: "announcement", title: "People are waiting to join", body: "Nobody who can let them in is in the meeting right now." });
    } else if (type === "poll.started") {
      emitNotice({ id: event.eventId, kind: "poll", title: "New poll", body: String(payload.title ?? ""), panel: "polls", action: "Vote" });
    }
    if (type === "breakout.participant_moved" && payload.participantId) {
      window.dispatchEvent(new CustomEvent(MEDIA_MOVE_EVENT, { detail: { participantId: String(payload.participantId) } }));
    }
    if (type.startsWith("transcription.")) {
      void queryClient.invalidateQueries({ queryKey: conferenceKeys.detail(conferenceId) });
    }
    if (type.startsWith("conference.")) {
      void queryClient.invalidateQueries({ queryKey: conferenceKeys.detail(conferenceId) });
      void queryClient.invalidateQueries({ queryKey: conferenceKeys.lists() });
    } else if (type.startsWith("participant.")) {
      void queryClient.invalidateQueries({ queryKey: participantKeys.all });
      void queryClient.invalidateQueries({ queryKey: collaborationKeys.state(conferenceId) });
    } else if (type.startsWith("chat.")) {
      void queryClient.invalidateQueries({ queryKey: [...collaborationKeys.all, "messages", conferenceId] });
      void queryClient.invalidateQueries({ queryKey: collaborationKeys.state(conferenceId) });
    } else if (type.startsWith("breakout.")) {
      void queryClient.invalidateQueries({ queryKey: breakoutKeys.list(conferenceId) });
    } else if (type.startsWith("poll.") || type.startsWith("qna.")) {
      void queryClient.invalidateQueries({ queryKey: engagementKeys.all });
    } else if (type.startsWith("recording.")) {
      // Keep the REC badge exact for everyone: refresh the session's recording state.
      void queryClient.invalidateQueries({ queryKey: conferenceKeys.detail(conferenceId) });
      void queryClient.invalidateQueries({ queryKey: ["v2"] });
    } else if (type.startsWith("collaboration.") || type.startsWith("hand.")) {
      void queryClient.invalidateQueries({ queryKey: collaborationKeys.state(conferenceId) });
    } else {
      void queryClient.invalidateQueries({ queryKey: ["v2"] });
    }
  }, [conferenceId, queryClient]);

  const handleCanonicalState = useCallback((conference: ConferenceResponse) => {
    queryClient.setQueryData(conferenceKeys.detail(conferenceId), conference);
    void queryClient.invalidateQueries({ queryKey: participantKeys.all });
    void queryClient.invalidateQueries({ queryKey: collaborationKeys.all });
    void queryClient.invalidateQueries({ queryKey: breakoutKeys.all });
    void queryClient.invalidateQueries({ queryKey: engagementKeys.all });
  }, [conferenceId, queryClient]);

  useEffect(() => () => markRealtime(conferenceId, false), [conferenceId]);

  useRealtime({
    conferenceId,
    enabled: !!conferenceId,
    onEvent: handleEvent,
    onCanonicalState: handleCanonicalState,
    // While events flow, the room's polls slow to a safety net (lib/realtime-health).
    onOpen: () => markRealtime(conferenceId, true),
    onClose: () => markRealtime(conferenceId, false),
    onError: error => console.error("Conference realtime connection error:", error),
  });

  return null;
}
