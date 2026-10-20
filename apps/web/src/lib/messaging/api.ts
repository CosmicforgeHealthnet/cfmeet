/**
 * CF Meet web client → Meeting Service → Messaging (Part V §104).
 *
 * Every Messaging operation goes through the Meeting Service, which verifies the CF Meet
 * login and acts for the account's own Messaging principal (signed into its service
 * token): `/api/v2/messaging/relay/v1/...` for the allowlisted end-user operations and
 * `/api/v2/messaging/{me,directory,...}` for identity. The browser never holds a
 * Messaging or Platform Access credential; the only Messaging token it ever sees is the
 * short-lived, device-bound client token for the realtime gateway.
 *
 * Requests carry the login bearer only (never a meeting participant token), so the
 * relay cannot confuse a guest identity with the signed-in account.
 */
import axios, { AxiosError, type AxiosInstance } from "axios";

import { cookieStore } from "@/store";

export const V2_BASE_URL = process.env.NEXT_PUBLIC_API_URL
  ? process.env.NEXT_PUBLIC_API_URL.replace("/api/v1", "/api/v2")
  : "http://localhost:3001/api/v2";
const V1_BASE_URL = V2_BASE_URL.replace("/api/v2", "/api/v1");

export class MessagingApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    public readonly reason: string | null,
  ) {
    super(code);
    this.name = "MessagingApiError";
  }
}

let refreshing: Promise<string> | null = null;
async function refreshLogin(): Promise<string> {
  refreshing ??= (async () => {
    const refreshToken = cookieStore.getRefreshToken();
    if (!refreshToken) throw new MessagingApiError(401, "AUTHENTICATION_REQUIRED", null);
    const { data } = await axios.post(`${V1_BASE_URL}/auth/refresh`, { refresh_token: refreshToken });
    cookieStore.setTokens(data.access_token, data.refresh_token);
    return data.access_token as string;
  })().finally(() => {
    refreshing = null;
  });
  return refreshing;
}

function create(): AxiosInstance {
  const http = axios.create({ baseURL: V2_BASE_URL, timeout: 30_000 });
  http.interceptors.request.use((config) => {
    const token = cookieStore.getAccessToken();
    if (token) config.headers.Authorization = `Bearer ${token}`;
    return config;
  });
  http.interceptors.response.use(undefined, async (error: AxiosError) => {
    const original = error.config as (typeof error.config & { _retried?: boolean }) | undefined;
    if (error.response?.status === 401 && original && !original._retried) {
      original._retried = true;
      const token = await refreshLogin();
      original.headers.Authorization = `Bearer ${token}`;
      return http(original);
    }
    throw error;
  });
  return http;
}

const http = create();

function asApiError(e: unknown): MessagingApiError {
  if (e instanceof MessagingApiError) return e;
  const ax = e as AxiosError<{ error?: { code?: string; reason?: string } }>;
  const status = ax.response?.status ?? 0;
  const code = ax.response?.data?.error?.code ?? (status ? `HTTP_${status}` : "NETWORK_UNAVAILABLE");
  return new MessagingApiError(status, code, ax.response?.data?.error?.reason ?? null);
}

async function call<T>(method: "GET" | "POST" | "PUT" | "DELETE", path: string, body?: unknown, headers?: Record<string, string>): Promise<T> {
  try {
    const r = await http.request<T>({ method, url: path, data: body, headers });
    return r.data;
  } catch (e) {
    throw asApiError(e);
  }
}

/** One allowlisted Messaging operation (`path` under `v1/`, query included). */
export function relay<T>(method: "GET" | "POST" | "PUT" | "DELETE", path: string, body?: unknown, idempotencyKey?: string): Promise<T> {
  if (!/^v1\/[A-Za-z0-9/_?&=.-]+$/.test(path)) throw new MessagingApiError(0, "BAD_PATH", null);
  return call<T>(method, `/messaging/relay/${path}`, body, idempotencyKey ? { "Idempotency-Key": idempotencyKey } : undefined);
}

export type MessagingIdentity = { principalId: string; tenantId: string; applicationId: string; environmentId: string };
export type Colleague = { userId: string; displayName: string; principalId: string | null };

export const identityApi = {
  me: () => call<MessagingIdentity>("GET", "/messaging/me"),
  directory: (q?: string) => call<{ people: Colleague[] }>("GET", `/messaging/directory${q ? `?q=${encodeURIComponent(q)}` : ""}`),
  resolve: (userIds: string[]) => call<{ people: { userId: string; principalId: string }[] }>("POST", "/messaging/directory/resolve", { userIds }),
  names: (principalIds: string[]) =>
    call<{ principals: { principalId: string; displayName: string }[] }>("POST", "/messaging/principals/names", { principalIds }),
  registrationChallenge: () => call<{ challengeId: string; deviceId: string; nonce: string; expiresAt: string }>("POST", "/messaging/devices/registration-challenges", {}),
  registerDevice: (challengeId: string, keyPackage: string, displayLabel: string) =>
    call<{ deviceId: string; status: string; signaturePublicKey: string }>("POST", "/messaging/devices", { challengeId, keyPackage, displayLabel }),
  publishKeyPackages: (deviceId: string, keyPackages: string[]) => call<unknown>("POST", `/messaging/devices/${deviceId}/key-packages`, { keyPackages }),
  revokeDevice: (deviceId: string) => call<unknown>("POST", `/messaging/devices/${deviceId}/revoke`),
  clientToken: (deviceId: string) => call<{ token: string; tokenType: string; sessionId: string; deviceId: string; expiresAt: string }>("POST", "/messaging/client-tokens", { deviceId }),
};

export const b64u = {
  enc(b: Uint8Array): string {
    let s = "";
    for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
    return btoa(s).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  },
  dec(s: string): Uint8Array {
    if (!/^[A-Za-z0-9_-]*$/.test(s)) throw new MessagingApiError(0, "BAD_BASE64", null);
    const bin = atob(s.replaceAll("-", "+").replaceAll("_", "/") + "===".slice((s.length + 3) % 4));
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  },
};
