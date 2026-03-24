import { useMutation, useQueryClient } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

export function useDeleteMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => meetingService.deleteMeeting(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["meetings"] });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Failed to delete meeting";
      console.error("Delete meeting error:", message);
    },
  });
}
