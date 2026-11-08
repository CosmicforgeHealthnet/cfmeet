import axios, { type AxiosError } from "axios";
import { accountSession } from "@/lib/auth/session";
import { MessagingApiError } from "@/lib/messaging/errors";
import { messagingApiUrls } from "@/lib/messaging/urls";
import type { MessagingMethod } from "@/lib/messaging/routes";

const urls = messagingApiUrls(process.env.NEXT_PUBLIC_API_URL || undefined);
export const MESSAGING_V2_BASE_URL = urls.v2;
// Deliberately separate from apiV2: a guest's X-Participant-Token must never
// accompany the signed-in user's Unified Messaging operations.
const http = axios.create({ baseURL: urls.v2, timeout: 30_000 });

// The in-memory account access token (ACC-019); refresh goes through the httpOnly session
// cookie and is shared (single-flight, cross-tab serialized) with every other client.
http.interceptors.request.use(async (config) => {
  const token = await accountSession.getAccessToken().catch(() => accountSession.peekAccessToken());
  if (!token) throw new MessagingApiError(401, "AUTHENTICATION_REQUIRED", null);
  config.headers.Authorization = `Bearer ${token}`;
  return config;
});

http.interceptors.response.use(undefined, async (error: AxiosError) => {
  const original = error.config as (typeof error.config & { _retried?: boolean }) | undefined;
  if (error.response?.status === 401 && original && !original._retried) {
    original._retried = true;
    let token: string;
    try {
      token = await accountSession.refreshAccessToken();
    } catch {
      throw new MessagingApiError(401, "AUTHENTICATION_REQUIRED", null);
    }
    original.headers.Authorization = `Bearer ${token}`;
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
