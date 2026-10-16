import { create } from "zustand";
import type { AuthState, UserInfo } from "@/features/auth/types";

export const useAuthStore = create<AuthState>((set) => ({
  user: null,
  isAuthenticated: false,
  isLoading: true,

  setUser: (user: UserInfo | null) => set({ user }),

  setIsAuthenticated: (isAuthenticated: boolean) => set({ isAuthenticated }),

  setIsLoading: (isLoading: boolean) => set({ isLoading }),

  logout: () =>
    set({
      user: null,
      isAuthenticated: false,
      isLoading: false,
    }),
}));
