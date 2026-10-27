"use client";

import { useCallback } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useRealtime } from "@/hooks/v2/useRealtime";
import { conferenceKeys } from "@/hooks/v2/useConferences";
import { participantKeys } from "@/hooks/v2/useParticipants";
import { collaborationKeys } from "@/hooks/v2/useCollaboration";
import { breakoutKeys } from "@/hooks/v2/useBreakouts";
import { engagementKeys } from "@/hooks/v2/useEngagement";
import type { ConferenceResponse, RealtimeEnvelope } from "@/types/v2";

export default function RealtimeRoomSync({ conferenceId }: { conferenceId: string }) {
  const queryClient = useQueryClient();

  const handleEvent = useCallback((event: RealtimeEnvelope) => {
    const type = event.eventType;
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

  useRealtime({
    conferenceId,
    enabled: !!conferenceId,
    onEvent: handleEvent,
    onCanonicalState: handleCanonicalState,
    onError: error => console.error("Conference realtime connection error:", error),
  });

  return null;
}
