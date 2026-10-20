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
  // V2 Query Keys
  V2: {
    SPACES: ["v2", "spaces"] as const,
    SPACE: (id: string) => ["v2", "space", id] as const,
    CONFERENCES: (spaceId: string) => ["v2", "conferences", spaceId] as const,
    CONFERENCE: (conferenceId: string) => ["v2", "conference", conferenceId] as const,
    PARTICIPANTS: (conferenceId: string) => ["v2", "participants", conferenceId] as const,
    WAITING_ROOM: (conferenceId: string) => ["v2", "waiting-room", conferenceId] as const,
    MESSAGES: (conferenceId: string) => ["v2", "messages", conferenceId] as const,
    BREAKOUTS: (conferenceId: string) => ["v2", "breakouts", conferenceId] as const,
    POLLS: (conferenceId: string) => ["v2", "polls", conferenceId] as const,
    POLL: (conferenceId: string, pollId: string) => ["v2", "poll", conferenceId, pollId] as const,
    POLL_RESULTS: (conferenceId: string, pollId: string) => ["v2", "poll-results", conferenceId, pollId] as const,
    QNA: (conferenceId: string) => ["v2", "qna", conferenceId] as const,
    COLLABORATION: (conferenceId: string) => ["v2", "collaboration", conferenceId] as const,
    RAISED_HANDS: (conferenceId: string) => ["v2", "raised-hands", conferenceId] as const,
    RESOLVE: (codeOrId: string) => ["v2", "resolve", codeOrId] as const,
    PRE_JOIN: (conferenceId: string) => ["v2", "pre-join", conferenceId] as const,
  },
} as const;
