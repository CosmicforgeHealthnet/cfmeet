import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { apiKeysService } from "@/services/api-keys.service";
import { AxiosError } from "axios";
import { ApiErrorResponse } from "@/types/auth";

export function useApiKeys() {
  return useQuery({
    queryKey: ["api-keys"],
    queryFn: async () => {
      const response = await apiKeysService.getAll();
      return response.data;
    },
    staleTime: Infinity, // never becomes stale
    gcTime: 1000 * 60 * 10, // keep cache for 10 mins
    refetchOnWindowFocus: false, // User switched tabs? Don’t refetch.
    refetchOnReconnect: false, //User switched tabs? Don’t refetch.
    refetchOnMount: false, // Component remounted? Don’t refetch
  });
}

export function useCreateApiKey() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async () => {
      const response = await apiKeysService.create();
      return response.data;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["api-keys"] });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      console.error(
        "Create API Key error:",
        error.response?.data?.error?.message,
      );
    },
  });
}

export function useRevokeApiKey() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (id: string) => {
      const response = await apiKeysService.revoke(id);
      return response;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["api-keys"] });
    },
    onError: (error: AxiosError<ApiErrorResponse>) => {
      console.error(
        "Revoke API Key error:",
        error.response?.data?.error?.message,
      );
    },
  });
}
