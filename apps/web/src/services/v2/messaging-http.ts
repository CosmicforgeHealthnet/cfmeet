import axios, { type AxiosError } from "axios";
import { cookieStore } from "@/store";
import { MessagingApiError } from "@/lib/messaging/errors";
import { messagingApiUrls } from "@/lib/messaging/urls";
import type { MessagingMethod } from "@/lib/messaging/routes";

const urls = messagingApiUrls(process.env.NEXT_PUBLIC_API_URL || undefined);
export const MESSAGING_V2_BASE_URL = urls.v2;
// Deliberately separate from apiV2: a guest's X-Participant-Token must never
// accompany the signed-in user's Unified Messaging operations.
const http = axios.create({ baseURL: urls.v2, timeout: 30_000 });
let refreshing: Promise<string> | null = null;

http.interceptors.request.use((config) => {
  const token = cookieStore.getAccessToken();
  if (!token) throw new MessagingApiError(401, "AUTHENTICATION_REQUIRED", null);
  config.headers.Authorization = `Bearer ${token}`;
  return config;
});

http.interceptors.response.use(undefined, async (error: AxiosError) => {
  const original = error.config as (typeof error.config & { _retried?: boolean }) | undefined;
  if (error.response?.status === 401 && original && !original._retried) {
    original._retried = true;
    refreshing ??= (async () => {
      const token = cookieStore.getRefreshToken();
      if (!token) throw new MessagingApiError(401, "AUTHENTICATION_REQUIRED", null);
      const { data } = await axios.post<{ access_token: string; refresh_token: string }>(`${urls.v1}/auth/refresh`, { refresh_token: token });
      cookieStore.setTokens(data.access_token, data.refresh_token);
      return data.access_token;
    })().finally(() => { refreshing = null; });
    original.headers.Authorization = `Bearer ${await refreshing}`;
    return http(original);
  }
  throw error;
});

export async function messagingRequest<T>(method: MessagingMethod, url: string, data?: unknown, headers?: Record<string, string>): Promise<T> {
  try {
    return (await http.request<T>({ method, url, data, headers })).data;
  } catch (error) {
    if (error instanceof MessagingApiError) throw error;
    if (axios.isAxiosError<{ error?: { code?: string; reason?: string; message?: string } }>(error)) {
      const status = error.response?.status ?? 0;
      const detail = error.response?.data?.error;
      throw new MessagingApiError(status, detail?.code ?? (status ? `HTTP_${status}` : "NETWORK_UNAVAILABLE"), detail?.reason ?? null, detail?.message);
    }
    throw new MessagingApiError(0, "NETWORK_UNAVAILABLE", null);
  }
}
