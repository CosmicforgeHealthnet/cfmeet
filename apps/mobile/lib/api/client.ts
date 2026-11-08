import axios, { AxiosError, AxiosInstance, InternalAxiosRequestConfig } from "axios";
import { API_URL, API_V2_URL } from "@/lib/constants";
import { tokenStorage, clearAllStorage } from "@/lib/storage/secure-store";
import {
  participantCredentialStore,
  conferenceIdFromPath,
} from "@/lib/storage/participant-credential";
import { decodeApiError, type ApiError } from "@/lib/api/errors";
import { router } from "expo-router";

export { toApiRequestError, ApiRequestError } from "@/lib/api/errors";
export type { ApiError, ApiErrorKind } from "@/lib/api/errors";

interface RefreshTokenResponse {
  access_token: string;
  refresh_token: string;
}

interface QueueItem {
  resolve: (value: unknown) => void;
  reject: (error: unknown) => void;
}

type RetriableConfig = InternalAxiosRequestConfig & { _retry?: boolean; _bearer?: boolean };

let isRefreshing = false;
let failedQueue: QueueItem[] = [];

const processQueue = (error: Error | null, token: string | null = null): void => {
  failedQueue.forEach((prom) => {
    if (error) {
      prom.reject(error);
    } else {
      prom.resolve(token);
    }
  });
  failedQueue = [];
};

const create = (baseURL: string) =>
  axios.create({
    baseURL,
    headers: { "Content-Type": "application/json" },
    timeout: 30000,
  });

/** v1 (auth, users) with the account bearer token. */
export const api = create(API_URL);
/** v1 without credentials (sign-in, sign-up, password reset). */
export const publicApi = create(API_URL);
/**
 * v2 meeting API. Sends the account bearer token when signed in; otherwise, on calls scoped
 * to the conference the guest joined, the participant credential (`X-Participant-Token`).
 */
export const apiV2 = create(API_V2_URL);
/** v2 without credentials. */
export const publicApiV2 = create(API_V2_URL);

const attachCredentials = (instance: AxiosInstance, participantScoped: boolean) => {
  instance.interceptors.request.use(
    async (config: InternalAxiosRequestConfig) => {
      const retriable = config as RetriableConfig;
      const token = await tokenStorage.getAccessToken();
      if (token && config.headers) {
        config.headers.Authorization = `Bearer ${token}`;
        retriable._bearer = true;
      } else if (participantScoped && config.headers) {
        // The backend never mixes identities: a bearer token wins, so the participant
        // credential is only sent by callers without an account, and only to the
        // conference it was issued for.
        const conferenceId = conferenceIdFromPath(config.url);
        const credential = conferenceId
          ? await participantCredentialStore.getFor(conferenceId)
          : null;
        if (credential) {
          config.headers["X-Participant-Token"] = credential;
        }
      }
      return config;
    },
    (error: AxiosError) => Promise.reject(error)
  );

  instance.interceptors.response.use(
    (response) => response,
    async (error: AxiosError) => {
      const originalRequest = error.config as RetriableConfig | undefined;

      if (!originalRequest) {
        return Promise.reject(error);
      }

      const isAuthEndpoint = originalRequest.url?.includes("/auth/");

      // Only an account session can be refreshed. A guest (participant credential or
      // anonymous) 401 is returned to the caller instead of bouncing to the login screen.
      if (
        error.response?.status === 401 &&
        !originalRequest._retry &&
        !isAuthEndpoint &&
        originalRequest._bearer
      ) {
        if (isRefreshing) {
          return new Promise((resolve, reject) => {
            failedQueue.push({ resolve, reject });
          }).then((token) => {
            if (originalRequest.headers) {
              originalRequest.headers.Authorization = `Bearer ${token}`;
            }
            return instance(originalRequest);
          });
        }

        originalRequest._retry = true;
        isRefreshing = true;

        try {
          const refreshToken = await tokenStorage.getRefreshToken();

          if (!refreshToken) {
            throw new Error("No refresh token available");
          }

          const response = await publicApi.post<RefreshTokenResponse>("/auth/refresh", {
            refresh_token: refreshToken,
          });

          const { access_token, refresh_token } = response.data;
          await tokenStorage.setTokens(access_token, refresh_token);

          processQueue(null, access_token);

          if (originalRequest.headers) {
            originalRequest.headers.Authorization = `Bearer ${access_token}`;
          }

          return instance(originalRequest);
        } catch (refreshError) {
          processQueue(refreshError as Error, null);
          await clearAllStorage();
          router.replace("/login");
          return Promise.reject(error);
        } finally {
          isRefreshing = false;
        }
      }

      return Promise.reject(error);
    }
  );
};

attachCredentials(api, false);
attachCredentials(apiV2, true);

/** Decode any API failure (v2 error envelope, legacy shapes, network errors). */
export const getApiError = (error: unknown): ApiError => decodeApiError(error);
