/**
 * V2 API Client
 * Base axios instance configured for the V2 API endpoints
 */

import axios from "axios";
import { cookieStore, storageStore } from "@/store";

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

// Request Interceptor: Attach the Access Token or Participant Token
apiV2.interceptors.request.use(
  (config) => {
    const accessToken = cookieStore.getAccessToken();
    const participantToken = storageStore.getParticipantToken?.();
    const boundConference = storageStore.getV2JoinData?.<{ conference?: { id?: string } }>()?.conference?.id;
    const conferencePath = config.url?.match(/^\/conferences\/([^/]+)(?:\/|$)/)?.[1];
    const tokenMatchesScope = !boundConference || conferencePath === encodeURIComponent(boundConference);

    if (accessToken) {
      config.headers.Authorization = `Bearer ${accessToken}`;
    }

    // For participant-scoped requests, add the X-Participant-Token header
    const accountOperation = /^\/(meeting-spaces(?:\/|$)|instant-meetings(?:\/|$)|resolve\/|organizations(?:\/|$)|me\/organizations(?:\/|$)|admin\/organizations(?:\/|$))/.test(config.url ?? "");
    if (participantToken && !accountOperation && tokenMatchesScope) {
      config.headers["X-Participant-Token"] = participantToken;
    }
    if (accountOperation) delete config.headers["X-Participant-Token"];

    return config;
  },
  (error) => Promise.reject(error),
);

// Response Interceptor: Auto-refresh on 401
let isRefreshing = false;
let failedQueue: Array<{
  resolve: (token: string) => void;
  reject: (error: unknown) => void;
}> = [];

const processQueue = (error: unknown, token: string | null = null) => {
  failedQueue.forEach((prom) => {
    if (token) {
      prom.resolve(token);
    } else {
      prom.reject(error);
    }
  });
  failedQueue = [];
};

apiV2.interceptors.response.use(
  (response) => response,
  async (error) => {
    const originalRequest = error.config;

    const isAuthEndpoint =
      originalRequest?.url?.includes("/auth/login") ||
      originalRequest?.url?.includes("/auth/refresh") ||
      originalRequest?.url?.includes("/auth/register");

    if (
      error.response?.status !== 401 ||
      !originalRequest ||
      originalRequest._retry ||
      isAuthEndpoint
    ) {
      return Promise.reject(error);
    }

    originalRequest._retry = true;
    if (isRefreshing) {
      return new Promise((resolve, reject) => {
        failedQueue.push({
          resolve: (token: string) => {
            originalRequest.headers.Authorization = `Bearer ${token}`;
            resolve(apiV2(originalRequest));
          },
          reject,
        });
      });
    }

    originalRequest._retry = true;
    isRefreshing = true;

    try {
      const refreshToken = cookieStore.getRefreshToken();
      if (!refreshToken) throw new Error("No refresh token");

      // Use V1 endpoint for token refresh (auth is still V1)
      const v1BaseUrl = V2_BASE_URL.replace("/api/v2", "/api/v1");
      const { data } = await axios.post(
        `${v1BaseUrl}/auth/refresh`,
        { refresh_token: refreshToken },
        { headers: { "Content-Type": "application/json" } },
      );

      const { access_token, refresh_token } = data;
      cookieStore.setTokens(access_token, refresh_token);
      processQueue(null, access_token);

      originalRequest.headers.Authorization = `Bearer ${access_token}`;
      return apiV2(originalRequest);
    } catch (refreshError) {
      processQueue(refreshError, null);
      cookieStore.clearTokens();
      storageStore.clearUser();
      return Promise.reject(refreshError);
    } finally {
      isRefreshing = false;
    }
  },
);

export default apiV2;
