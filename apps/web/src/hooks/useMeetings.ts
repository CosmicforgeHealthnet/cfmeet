import { useQuery } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";
import type { MeetingsParams } from "@/types/meeting";

export function useMeetings(params?: MeetingsParams) {
  return useQuery({
    queryKey: ["meetings", params],
    queryFn: () => meetingService.getMeetings(params),
    // 👇 key settings
    staleTime: Infinity, // never becomes stale
    gcTime: 1000 * 60 * 10, // keep cache for 10 mins
    refetchOnWindowFocus: false, // User switched tabs? Don’t refetch.
    refetchOnReconnect: false, //User switched tabs? Don’t refetch.
    // refetchOnMount: false, // Component remounted? Don’t refetch
  });
}
