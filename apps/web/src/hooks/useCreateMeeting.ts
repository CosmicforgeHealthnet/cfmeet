import { useMutation, useQueryClient } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";
import type { CreateMeetingRequest } from "@/types/meeting";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

export function useCreateMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: CreateMeetingRequest) =>
      meetingService.createMeeting(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["meetings"] });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Failed to create meeting";
      console.error("Create meeting error:", message);
    },
  });
}
