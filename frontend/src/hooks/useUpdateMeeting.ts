import { useMutation, useQueryClient } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";
import type { UpdateMeetingRequest } from "@/types/meeting";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

interface UpdateMeetingParams {
  id: string;
  data: UpdateMeetingRequest;
}

export function useUpdateMeeting() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({ id, data }: UpdateMeetingParams) =>
      meetingService.updateMeeting(id, data),
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ["meetings"] });
      queryClient.invalidateQueries({ queryKey: ["meeting", variables.id] });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      const message =
        error.response?.data?.error?.message || "Failed to update meeting";
      console.error("Update meeting error:", message);
    },
  });
}
