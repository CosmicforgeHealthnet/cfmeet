/**
 * First-party Meeting API client for the calling surfaces that Meeting relays:
 *  - Communications Hub call launch / contacts / call history (`/api/v2/communications/*`);
 *  - per-organization Enterprise Calling (`/api/v2/organizations/{id}/calling/{path}`).
 *
 * Deliberately separate from apiV2: a guest's X-Participant-Token must never accompany the
 * signed-in user's calling operations. The account access token is attached (and refreshed
 * once on 401) by the shared session core.
 */

import axios from "axios";
import { attachAccountSession } from "@/lib/auth/axios-session";

export const MEETING_V2_BASE_URL = process.env.NEXT_PUBLIC_API_URL
  ? process.env.NEXT_PUBLIC_API_URL.replace(/\/api\/v1\/?$/, "/api/v2")
  : "http://localhost:8080/api/v2";

export const meetingCallingApi = axios.create({
  baseURL: MEETING_V2_BASE_URL,
  timeout: 30_000,
  headers: { "Content-Type": "application/json" },
});

attachAccountSession(meetingCallingApi);
