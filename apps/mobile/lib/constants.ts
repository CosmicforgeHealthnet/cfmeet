import Constants from "expo-constants";

const extra = Constants.expoConfig?.extra;

export const API_URL = extra?.apiUrl || process.env.EXPO_PUBLIC_API_URL || "https://api.cfmeet.com/api/v1";
export const SOCKET_URL = extra?.socketUrl || "https://api.cfmeet.com";

export const STORAGE_KEYS = {
  ACCESS_TOKEN: "access_token",
  REFRESH_TOKEN: "refresh_token",
  USER_INFO: "user_info",
  REMEMBER_ME: "remember_me",
} as const;

export const QUERY_KEYS = {
  USER: ["user"] as const,
  MEETINGS: ["meetings"] as const,
  MEETING: (id: string) => ["meeting", id] as const,
  PUBLIC_MEETING: (identifier: string) => ["public-meeting", identifier] as const,
  PARTICIPANTS: (meetingId: string) => ["participants", meetingId] as const,
  WAITING_PARTICIPANTS: (meetingId: string) => ["waiting-participants", meetingId] as const,
  CHAT: (meetingId: string) => ["chat", meetingId] as const,
  API_KEYS: ["api-keys"] as const,
  WEBHOOKS: ["webhooks"] as const,
} as const;
