import { useQuery } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";

export function useParticipants(meetingId: string, enabled = true) {
  return useQuery({
    queryKey: ["participants", meetingId],
    queryFn: () => meetingService.getParticipants(meetingId),
    enabled: !!meetingId && enabled,
    refetchInterval: 5000, // Poll every 5 seconds for participant updates
  });
}
