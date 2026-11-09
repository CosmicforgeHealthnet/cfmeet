import { useMutation, useQueryClient } from "@tanstack/react-query";
import { authService } from "@/services/auth.service";

export function useLogout() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => authService.logout(),
    onSettled: async () => {
      await queryClient.cancelQueries({ queryKey: ["calling"] });
      queryClient.removeQueries({ queryKey: ["calling"] });
      await queryClient.cancelQueries({ queryKey: ["v2", "messaging"] });
      queryClient.removeQueries({ queryKey: ["v2", "messaging"] });
      await queryClient.cancelQueries({ queryKey: ["v2", "chat-migration"] });
      queryClient.removeQueries({ queryKey: ["v2", "chat-migration"] });
      // Nothing of the signed-out account (profile, workspaces, organizations) stays cached.
      queryClient.clear();
    },
    onError: (error) => {
      console.error("Logout error:", error);
    },
  });
}
