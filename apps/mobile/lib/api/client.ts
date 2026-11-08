import axios, {
  AxiosError,
  AxiosInstance,
  AxiosRequestConfig,
  InternalAxiosRequestConfig,
} from "axios";
import { API_URL, API_V2_URL } from "@/lib/constants";
import { tokenStorage } from "@/lib/storage/secure-store";
import {
  participantCredentialStore,
  conferenceIdFromPath,
} from "@/lib/storage/participant-credential";
import { decodeApiError, toApiRequestError, type ApiError } from "@/lib/api/errors";
import { ROUTES, type Route } from "@/lib/api/routes";
import type { RefreshTokenResponse } from "@/lib/api/generated/meeting-service";

export { toApiRequestError, ApiRequestError } from "@/lib/api/errors";
export type { ApiError, ApiErrorKind } from "@/lib/api/errors";

/*
 * Security rules (ACC-012, FE-005):
 * - Nothing in this module logs a request, a response, a header or a token.
 * - The account bearer token goes only to the API host from the build configuration.
 * - The participant credential (`X-Participant-Token`) is sent only by callers without an
 *   account session, and only on calls scoped to the conference it was issued for.
 * - Only a 4xx answer to the refresh call ends the session; a network failure does not sign the
 *   user out.
 */

interface QueueItem {
  resolve: (token: string) => void;
  reject: (error: unknown) => void;
}

type RetriableConfig = InternalAxiosRequestConfig & { _retry?: boolean; _bearer?: boolean };

let isRefreshing = false;
let failedQueue: QueueItem[] = [];
let sessionEndedHandler: (() => void | Promise<void>) | null = null;

/**
 * Registers what happens when the account session can no longer be refreshed (sign-out of the
 * local state: tokens, per-context caches, navigation). Set once by the auth provider.
 */
export function onSessionEnded(handler: () => void | Promise<void>): void {
  sessionEndedHandler = handler;
}

const processQueue = (error: unknown, token: string | null): void => {
  for (const item of failedQueue) {
    if (token) item.resolve(token);
    else item.reject(error);
  }
  failedQueue = [];
};

const create = (baseURL: string) =>
  axios.create({
    baseURL,
    headers: { "Content-Type": "application/json" },
    timeout: 30000,
  });

/** v1 with the account bearer token (account operations). */
const v1 = create(API_URL);
/** v1 without credentials (sign-in, sign-up, password reset, refresh). */
const v1Public = create(API_URL);
/** v2 with the account bearer token, or the participant credential for guests. */
const v2 = create(API_V2_URL);
/** v2 without credentials (public resolve). */
const v2Public = create(API_V2_URL);

const attachCredentials = (instance: AxiosInstance, participantScoped: boolean) => {
  instance.interceptors.request.use(async (config: InternalAxiosRequestConfig) => {
    const retriable = config as RetriableConfig;
    const token = await tokenStorage.getAccessToken();
    if (token) {
      config.headers.Authorization = `Bearer ${token}`;
      retriable._bearer = true;
    } else if (participantScoped) {
      const conferenceId = conferenceIdFromPath(config.url);
      const credential = conferenceId ? await participantCredentialStore.getFor(conferenceId) : null;
      if (credential) config.headers["X-Participant-Token"] = credential;
    }
    return config;
  });

  instance.interceptors.response.use(
    (response) => response,
    async (error: AxiosError) => {
      const original = error.config as RetriableConfig | undefined;
      if (!original || error.response?.status !== 401 || original._retry || !original._bearer) {
        return Promise.reject(error);
      }

      if (isRefreshing) {
        return new Promise<string>((resolve, reject) => {
          failedQueue.push({ resolve, reject });
        }).then((token) => {
          original._retry = true;
          original.headers.Authorization = `Bearer ${token}`;
          return instance(original);
        });
      }

      original._retry = true;
      isRefreshing = true;
      try {
        const refreshToken = await tokenStorage.getRefreshToken();
        if (!refreshToken) throw new SessionEndedError();
        let refreshed: RefreshTokenResponse;
        try {
          const response = await v1Public.post<RefreshTokenResponse>(ROUTES.refresh.path(), {
            refresh_token: refreshToken,
          });
          refreshed = response.data;
        } catch (refreshError) {
          const status = (refreshError as AxiosError).response?.status;
          // Only the server's refusal ends the session; offline keeps it for a later retry.
          if (status && status >= 400 && status < 500) throw new SessionEndedError();
          throw refreshError;
        }
        await tokenStorage.setTokens(refreshed.access_token, refreshed.refresh_token);
        processQueue(null, refreshed.access_token);
        original.headers.Authorization = `Bearer ${refreshed.access_token}`;
        return instance(original);
      } catch (refreshError) {
        processQueue(refreshError, null);
        if (refreshError instanceof SessionEndedError) {
          await sessionEndedHandler?.();
          return Promise.reject(error);
        }
        return Promise.reject(refreshError);
      } finally {
        isRefreshing = false;
      }
    }
  );
};

class SessionEndedError extends Error {
  constructor() {
    super("session ended");
  }
}

attachCredentials(v1, false);
attachCredentials(v2, true);

export interface SendOptions {
  data?: unknown;
  params?: Record<string, string | number | boolean | undefined>;
  headers?: Record<string, string>;
  /** Send without any credential (sign-in, public lookups). */
  anonymous?: boolean;
  signal?: AbortSignal;
}

function instanceFor(route: Route, anonymous: boolean): AxiosInstance {
  if (route.version === "v1") return anonymous ? v1Public : v1;
  return anonymous ? v2Public : v2;
}

/**
 * Performs one contract operation from {@link ROUTES}. Resolves with the response body;
 * rejects with an {@link ApiRequestError} carrying the decoded v2 error envelope.
 */
export async function send<T, A extends string[] = string[]>(
  route: Route<A>,
  args: A,
  options: SendOptions = {}
): Promise<T> {
  const config: AxiosRequestConfig = {
    method: route.method,
    url: route.path(...args),
    data: options.data,
    params: options.params,
    headers: options.headers,
    signal: options.signal,
  };
  try {
    const response = await instanceFor(route as Route, !!options.anonymous).request<T>(config);
    return response.data;
  } catch (error) {
    throw toApiRequestError(error);
  }
}

/** Same as {@link send}, but also returns the response headers (e.g. `Idempotent-Replayed`). */
export async function sendWithHeaders<T, A extends string[] = string[]>(
  route: Route<A>,
  args: A,
  options: SendOptions = {}
): Promise<{ data: T; headers: Record<string, string> }> {
  try {
    const response = await instanceFor(route as Route, !!options.anonymous).request<T>({
      method: route.method,
      url: route.path(...args),
      data: options.data,
      params: options.params,
      headers: options.headers,
      signal: options.signal,
    });
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(response.headers ?? {})) {
      if (typeof v === "string") headers[k.toLowerCase()] = v;
    }
    return { data: response.data, headers };
  } catch (error) {
    throw toApiRequestError(error);
  }
}

/** Decode any API failure (v2 error envelope, legacy shapes, network errors). */
export const getApiError = (error: unknown): ApiError => decodeApiError(error);
