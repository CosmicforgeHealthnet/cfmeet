import { useQuery } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";

export function useMeeting(id: string) {
  return useQuery({
    queryKey: ["meeting", id],
    queryFn: () => meetingService.getMeeting(id),
    enabled: !!id,
    refetchInterval: 5000,
  });
}
