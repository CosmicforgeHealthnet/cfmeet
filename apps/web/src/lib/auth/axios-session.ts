import type { AxiosError, AxiosInstance, InternalAxiosRequestConfig } from "axios";
import { accountSession, SessionEndedError } from "./session";
import { storageStore } from "@/store";

type RetriableConfig = InternalAxiosRequestConfig & { _sessionRetried?: boolean };

/** Endpoints whose 401 means "wrong credentials", never "refresh and retry". */
const AUTH_ENDPOINT = /\/auth\/(login|register|refresh|oauth\/google\/exchange)(\/|$|\?)/;

/**
 * Attaches the in-memory account access token to every request of `instance` and, on a
 * 401, refreshes once through the httpOnly session cookie and retries (ACC-019). Refresh is
 * single-flight and cross-tab serialized inside `accountSession`.
 */
export function attachAccountSession(instance: AxiosInstance): void {
  instance.interceptors.request.use(async (config) => {
    let token: string | null;
    try {
      token = await accountSession.getAccessToken();
    } catch {
      token = accountSession.peekAccessToken();
    }
    if (token) config.headers.Authorization = `Bearer ${token}`;
    return config;
  });

  instance.interceptors.response.use(undefined, async (error: AxiosError) => {
    const original = error.config as RetriableConfig | undefined;
    if (
      error.response?.status !== 401 ||
      !original ||
      original._sessionRetried ||
      AUTH_ENDPOINT.test(original.url ?? "") ||
      !accountSession.hasSession()
    ) {
      throw error;
    }
    original._sessionRetried = true;
    try {
      const token = await accountSession.refreshAccessToken();
      original.headers.Authorization = `Bearer ${token}`;
      return instance(original);
    } catch (refreshError) {
      if (refreshError instanceof SessionEndedError) storageStore.clearUser();
      throw error;
    }
  });
}
