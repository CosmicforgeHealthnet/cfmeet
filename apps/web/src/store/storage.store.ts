import type { UserInfo } from "@/types/auth";
import type { JoinMeetingData } from "@/types/meeting";

const USER_KEY = "cosmic_user_info";
const INSTANT_JOIN_KEY = "cosmic_instant_join";

export const storageStore = {
  setUser(user: UserInfo) {
    if (typeof window !== "undefined") {
      localStorage.setItem(USER_KEY, JSON.stringify(user));
    }
  },

  getUser(): UserInfo | null {
    if (typeof window !== "undefined") {
      const savedUser = localStorage.getItem(USER_KEY);
      return savedUser ? JSON.parse(savedUser) : null;
    }
    return null;
  },

  clearUser() {
    if (typeof window !== "undefined") {
      localStorage.removeItem(USER_KEY);
    }
  },

  // Instant meeting join data (temporary, used for navigating to room after creating instant meeting)
  setInstantJoinData(data: JoinMeetingData) {
    if (typeof window !== "undefined") {
      sessionStorage.setItem(INSTANT_JOIN_KEY, JSON.stringify(data));
    }
  },

  getInstantJoinData(): JoinMeetingData | null {
    if (typeof window !== "undefined") {
      const data = sessionStorage.getItem(INSTANT_JOIN_KEY);
      return data ? JSON.parse(data) : null;
    }
    return null;
  },

  clearInstantJoinData() {
    if (typeof window !== "undefined") {
      sessionStorage.removeItem(INSTANT_JOIN_KEY);
    }
  },
};
