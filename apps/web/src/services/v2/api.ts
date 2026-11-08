/**
 * V2 API Client
 * Base axios instance configured for the V2 API endpoints
 */

import axios from "axios";
import { storageStore } from "@/store";
import { attachAccountSession } from "@/lib/auth/axios-session";

const V2_BASE_URL = process.env.NEXT_PUBLIC_API_URL
  ? process.env.NEXT_PUBLIC_API_URL.replace("/api/v1", "/api/v2")
  : "http://localhost:8080/api/v2";

// Public V2 API instance (no auth interceptors)
export const publicApiV2 = axios.create({
  baseURL: V2_BASE_URL,
  headers: {
    "Content-Type": "application/json",
  },
});

// Authenticated V2 API instance
const apiV2 = axios.create({
  baseURL: V2_BASE_URL,
  headers: {
    "Content-Type": "application/json",
  },
});

/**
 * Account-scoped operations: never carry a guest's X-Participant-Token (the account
 * session alone authorizes them).
 */
const ACCOUNT_OPERATION =
  /^\/(meeting-spaces(?:\/|$)|instant-meetings(?:\/|$)|resolve\/|organizations(?:\/|$)|organization-invitations(?:\/|$)|me(?:\/|$)|admin\/organizations(?:\/|$))/;

// Participant token for conference-scoped guest requests.
apiV2.interceptors.request.use(
  (config) => {
    const participantToken = storageStore.getParticipantToken?.();
    const boundConference = storageStore.getV2JoinData?.<{ conference?: { id?: string } }>()?.conference?.id;
    const conferencePath = config.url?.match(/^\/conferences\/([^/]+)(?:\/|$)/)?.[1];
    const tokenMatchesScope = !boundConference || conferencePath === encodeURIComponent(boundConference);

    const accountOperation = ACCOUNT_OPERATION.test(config.url ?? "");
    if (participantToken && !accountOperation && tokenMatchesScope) {
      config.headers["X-Participant-Token"] = participantToken;
    }
    if (accountOperation) delete config.headers["X-Participant-Token"];

    return config;
  },
  (error) => Promise.reject(error),
);

// In-memory account access token + one refresh-and-retry on 401 (ACC-019).
attachAccountSession(apiV2);

export default apiV2;
