import { useQuery } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";

export function usePublicMeeting(meetingIdentifier: string) {
  return useQuery({
    queryKey: ["publicMeeting", meetingIdentifier],
    queryFn: () => meetingService.getPublicMeeting(meetingIdentifier),
    enabled: !!meetingIdentifier,
  });
}
