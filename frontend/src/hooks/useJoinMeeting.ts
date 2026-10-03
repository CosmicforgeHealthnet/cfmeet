import { useMutation, useQueryClient } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";
import type { ApiErrorResponse } from "@/types/auth";
import type { JoinMeetingRequest } from "@/types/meeting";
import { AxiosError } from "axios";

interface JoinMeetingParams {
  meetingIdentifier: string;
  data: JoinMeetingRequest;
}

export function useJoinMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({ meetingIdentifier, data }: JoinMeetingParams) =>
      meetingService.joinMeeting(meetingIdentifier, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["meetings"] });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Failed to join meeting";
      console.error("Join meeting error:", message);
    },
  });
}
