import Constants from "expo-constants";
import { resolveApiUrl } from "@/lib/config/api-url";

interface Extra {
  apiUrl?: string;
  appEnv?: string;
  helpUrl?: string;
  termsUrl?: string;
  privacyUrl?: string;
}

const extra = (Constants.expoConfig?.extra ?? {}) as Extra;

// FE-006: the build environment decides the API host; release builds refuse cleartext
// HTTP to non-loopback hosts and fall back to the HTTPS production default.
const resolvedApi = resolveApiUrl({
  envUrl: process.env.EXPO_PUBLIC_API_URL,
  extraUrl: extra.apiUrl,
  isDev: __DEV__,
});

if (resolvedApi.rejected) {
  console.warn(
    `Ignoring API URL ${resolvedApi.rejected.url}: ${resolvedApi.rejected.reason}. Using ${resolvedApi.apiUrl}.`
  );
}

export const API_URL = resolvedApi.apiUrl;
export const API_V2_URL = resolvedApi.apiV2Url;

function httpsOrNull(value: string | undefined): string | null {
  const v = value?.trim();
  return v && /^https:\/\/[^\s]+$/i.test(v) ? v : null;
}

/**
 * Support pages (FE-012). Owner inputs: shown only when configured (`EXPO_PUBLIC_HELP_URL`,
 * `EXPO_PUBLIC_TERMS_URL`, `EXPO_PUBLIC_PRIVACY_URL`, HTTPS only); never an inert row.
 */
export const SUPPORT_LINKS = {
  help: httpsOrNull(process.env.EXPO_PUBLIC_HELP_URL ?? extra.helpUrl),
  terms: httpsOrNull(process.env.EXPO_PUBLIC_TERMS_URL ?? extra.termsUrl),
  privacy: httpsOrNull(process.env.EXPO_PUBLIC_PRIVACY_URL ?? extra.privacyUrl),
} as const;

export const STORAGE_KEYS = {
  ACCESS_TOKEN: "access_token",
  REFRESH_TOKEN: "refresh_token",
  USER_INFO: "user_info",
  REMEMBER_ME: "remember_me",
  PARTICIPANT_CREDENTIAL: "participant_credential",
  WORKSPACE: "workspace_context",
} as const;

/**
 * Query keys. Everything that depends on the workspace context starts with
 * `["ctx", workspaceId]` (see lib/workspace/context.ts) so a removed context is purged at once.
 */
export const QUERY_KEYS = {
  PROFILE: ["me", "profile"] as const,
  SESSIONS: ["me", "sessions"] as const,
  WORKSPACES: ["me", "workspaces"] as const,
  ORGANIZATIONS: ["me", "organizations"] as const,
  WORKSPACE_CAPABILITIES: (workspaceId: string) => ["ctx", workspaceId, "capabilities"] as const,
  AGENDA: (workspaceId: string, from: string, to: string) => ["ctx", workspaceId, "agenda", from, to] as const,
  SPACES: ["meeting-spaces"] as const,
  SPACE: (id: string) => ["meeting-space", id] as const,
  SPACE_CONFERENCES: (id: string) => ["meeting-space", id, "conferences"] as const,
  RESOLVE: (identifier: string) => ["resolve", identifier] as const,
  PRE_JOIN: (conferenceId: string) => ["conference", conferenceId, "pre-join"] as const,
  CONFERENCE: (conferenceId: string) => ["conference", conferenceId] as const,
  PARTICIPANTS: (conferenceId: string) => ["conference", conferenceId, "participants"] as const,
  ME: (conferenceId: string) => ["conference", conferenceId, "me"] as const,
  WAITING_ROOM: (conferenceId: string) => ["conference", conferenceId, "waiting-room"] as const,
  MESSAGES: (conferenceId: string) => ["conference", conferenceId, "messages"] as const,
} as const;
