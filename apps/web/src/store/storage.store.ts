import type { UserInfo } from "@/types/auth";
import type { JoinMeetingData } from "@/types/meeting";

const USER_KEY = "cosmic_user_info";
const INSTANT_JOIN_KEY = "cosmic_instant_join";
const PARTICIPANT_TOKEN_KEY = "cosmic_participant_token";
const V2_JOIN_DATA_KEY = "cosmic_v2_join_data";

export const storageStore = {
  setUser(user: UserInfo) {
    if (typeof window !== "undefined") {
      localStorage.setItem(USER_KEY, JSON.stringify(user));
    }
  },

  getUser(): UserInfo | null {
    if (typeof window !== "undefined") {
      try {
        const savedUser = localStorage.getItem(USER_KEY);
        return savedUser ? JSON.parse(savedUser) : null;
      } catch {
        localStorage.removeItem(USER_KEY);
        return null;
      }
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
      try {
        const data = sessionStorage.getItem(INSTANT_JOIN_KEY);
        return data ? JSON.parse(data) : null;
      } catch {
        sessionStorage.removeItem(INSTANT_JOIN_KEY);
        return null;
      }
    }
    return null;
  },

  clearInstantJoinData() {
    if (typeof window !== "undefined") {
      sessionStorage.removeItem(INSTANT_JOIN_KEY);
    }
  },

  // V2 Participant token (for guest sessions in V2 API)
  setParticipantToken(token: string) {
    if (typeof window !== "undefined") {
      sessionStorage.setItem(PARTICIPANT_TOKEN_KEY, token);
    }
  },

  getParticipantToken(): string | null {
    if (typeof window !== "undefined") {
      return sessionStorage.getItem(PARTICIPANT_TOKEN_KEY);
    }
    return null;
  },

  clearParticipantToken() {
    if (typeof window !== "undefined") {
      sessionStorage.removeItem(PARTICIPANT_TOKEN_KEY);
    }
  },

  // V2 Join data (temporary, used for navigating to room after joining via V2 API)
  setV2JoinData(data: unknown) {
    if (typeof window !== "undefined") {
      sessionStorage.setItem(V2_JOIN_DATA_KEY, JSON.stringify(data));
    }
  },

  getV2JoinData<T = unknown>(): T | null {
    if (typeof window !== "undefined") {
      try {
        const data = sessionStorage.getItem(V2_JOIN_DATA_KEY);
        return data ? JSON.parse(data) : null;
      } catch {
        sessionStorage.removeItem(V2_JOIN_DATA_KEY);
        return null;
      }
    }
    return null;
  },

  clearV2JoinData() {
    if (typeof window !== "undefined") {
      sessionStorage.removeItem(V2_JOIN_DATA_KEY);
    }
  },
};
