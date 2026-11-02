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
import { cookieStore, storageStore } from "@/store";
import { callingOrigin } from "@/lib/calling/origin.mjs";

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

callingApi.interceptors.request.use(
  (config) => {
    const token = cookieStore.getAccessToken();
    if (token) {
      config.headers.Authorization = `Bearer ${token}`;
    }
    return config;
  },
  (error) => Promise.reject(error),
);

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

callingApi.interceptors.response.use(
  (response) => response,
  async (error) => {
    const originalRequest = error.config;

    if (error.response?.status !== 401 || !originalRequest || originalRequest._retry) {
      return Promise.reject(error);
    }

    originalRequest._retry = true;
    if (isRefreshing) {
      return new Promise((resolve, reject) => {
        failedQueue.push({
          resolve: (token: string) => {
            originalRequest.headers.Authorization = `Bearer ${token}`;
            resolve(callingApi(originalRequest));
          },
          reject,
        });
      });
    }

    isRefreshing = true;

    try {
      const refreshToken = cookieStore.getRefreshToken();
      if (!refreshToken) throw new Error("No refresh token");

      const base = process.env.NEXT_PUBLIC_API_URL || "http://localhost:8080/api/v1";
      const { data } = await axios.post(
        `${base}/auth/refresh`,
        { refresh_token: refreshToken },
        { headers: { "Content-Type": "application/json" } },
      );

      const { access_token, refresh_token } = data;
      cookieStore.setTokens(access_token, refresh_token);
      processQueue(null, access_token);

      originalRequest.headers.Authorization = `Bearer ${access_token}`;
      return callingApi(originalRequest);
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

export default callingApi;
