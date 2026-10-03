import { useMutation, useQueryClient } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

export function useEndMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => meetingService.endMeeting(id),
    onSuccess: (_data, id) => {
      queryClient.invalidateQueries({ queryKey: ["meetings"] });
      queryClient.invalidateQueries({ queryKey: ["meeting", id] });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Failed to end meeting";
      console.error("End meeting error:", message);
    },
  });
}
