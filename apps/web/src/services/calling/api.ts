/**
 * Calling API client (services/calling/).
 *
 * Two credential classes of the Calling service:
 *  - login credential to the same-origin adapter for the call API, endpoint
 *    registry, signaling-credential minting and history;
 *  - signaling credential (a short-lived, endpoint-scoped token) for the endpoint's
 *    own offer recovery / answers and its own session media credentials.
 *
 * The two never mix: the platform credential is never attached to signaling routes and
 * the signaling credential never reaches the platform routes.
 */

import axios from "axios";
import { callingOrigin } from "@/lib/calling/origin.mjs";
import { attachAccountSession } from "@/lib/auth/axios-session";

export const CALLING_BASE_URL =
  callingOrigin(process.env.NEXT_PUBLIC_CALLING_API_URL, process.env.NEXT_PUBLIC_API_URL,
    process.env.NODE_ENV === "production");

export const callingApi = axios.create({
  baseURL: "/api/calling",
  timeout: 30_000,
  headers: {
    "Content-Type": "application/json",
  },
});

// Login credential: the in-memory account access token, refreshed once on 401 through the
// httpOnly session cookie (ACC-019). The same-origin adapter verifies it server-side.
attachAccountSession(callingApi);

export default callingApi;
