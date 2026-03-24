import { useMutation } from "@tanstack/react-query";
import { authService } from "@/services/auth.service";

export function useLogout() {
  return useMutation({
    mutationFn: () => authService.logout(),
    onError: (error) => {
      console.error("Logout error:", error);
    },
  });
}
