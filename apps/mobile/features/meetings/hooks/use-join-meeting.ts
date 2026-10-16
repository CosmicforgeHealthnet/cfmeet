import { useMutation } from "@tanstack/react-query";
import { router } from "expo-router";
import { meetingsApi } from "../api/meetings.api";
import { tokenStorage } from "@/lib/storage/secure-store";
import { getApiError } from "@/lib/api/client";
import type { JoinMeetingRequest } from "../types";

interface JoinMeetingParams {
  identifier: string;
  data: JoinMeetingRequest;
  isGuest?: boolean;
}

export function useJoinMeeting() {
  return useMutation({
    mutationFn: async ({ identifier, data, isGuest }: JoinMeetingParams) => {
      try {
        if (isGuest) {
          return await meetingsApi.joinMeetingAsGuest(identifier, data);
        }
        return await meetingsApi.joinMeeting(identifier, data);
      } catch (error) {
        const apiError = getApiError(error);
        throw new Error(apiError.message);
      }
    },
    onSuccess: async (response) => {
      if (response.access_token) {
        await tokenStorage.setAccessToken(response.access_token);
      }

      router.push({
        pathname: "/(room)/[id]/live",
        params: {
          id: response.data.id,
          joinData: JSON.stringify(response.data),
        },
      });
    },
  });
}
