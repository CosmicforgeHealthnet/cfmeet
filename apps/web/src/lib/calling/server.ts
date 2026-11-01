import "server-only";

import type { Call, EndpointList } from "@/types/calling";

export class CallingHttpError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message);
  }
}

export type CallingIdentity = {
  principalId: string;
  tenantId: string;
  applicationId: string;
  environmentId: string;
};

export async function callingIdentity(request: Request): Promise<CallingIdentity> {
  const authorization = request.headers.get("authorization");
  if (!authorization?.startsWith("Bearer ")) {
    throw new CallingHttpError(401, "AUTHENTICATION_FAILED", "Sign in to use calling.");
  }
  const base = process.env.NEXT_PUBLIC_API_URL || "http://localhost:8080/api/v1";
  const response = await fetch(`${base.replace(/\/api\/v1\/?$/, "/api/v2")}/messaging/me`, {
    headers: { Authorization: authorization }, cache: "no-store", redirect: "error",
    signal: AbortSignal.timeout(15_000),
  });
  if (!response.ok) throw new CallingHttpError(response.status === 401 ? 401 : 503,
    "AUTHENTICATION_FAILED", "Your communication identity is unavailable.");
  const identity = await response.json() as CallingIdentity;
  // The configured credential must belong to exactly this identity scope. Never
  // promote an arbitrary signed-in account into the credential's application.
  if (!identity.principalId || !process.env.CALLING_PLATFORM_CREDENTIAL ||
    identity.tenantId !== process.env.CALLING_TENANT_ID ||
    identity.applicationId !== process.env.CALLING_APPLICATION_ID ||
    identity.environmentId !== process.env.CALLING_ENVIRONMENT_ID) {
    throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED", "Calling is not configured for this account.");
  }
  return identity;
}

export async function callingFetch(path: string, init: RequestInit = {}) {
  const configured = process.env.CALLING_API_URL || process.env.NEXT_PUBLIC_CALLING_API_URL;
  // Never silently fall back to a localhost Calling backend in a deployed app.
  // The adapter must only call the explicitly configured service over TLS.
  if (process.env.NODE_ENV === "production" && (!configured || !configured.startsWith("https://"))) {
    throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED", "Calling API is not securely configured.");
  }
  const base = configured || "http://localhost:8090";
  const headers = new Headers(init.headers);
  headers.set("Authorization", `Bearer ${process.env.CALLING_PLATFORM_CREDENTIAL}`);
  headers.set("Content-Type", "application/json");
  return fetch(`${base.replace(/\/$/, "")}${path}`, {
    ...init, headers, cache: "no-store", redirect: "error", signal: AbortSignal.timeout(20_000),
  });
}

export async function ownEndpoints(identity: CallingIdentity): Promise<EndpointList> {
  const response = await callingFetch(`/v1/calling/endpoints?principalId=${encodeURIComponent(identity.principalId)}`);
  if (!response.ok) throw new CallingHttpError(503, "SERVICE_UNAVAILABLE", "Cannot verify your calling devices.");
  return response.json();
}

export async function ownCall(identity: CallingIdentity, id: string): Promise<Call> {
  const response = await callingFetch(`/v1/calls/${id}`);
  if (!response.ok) throw new CallingHttpError(404, "NOT_FOUND", "Call not found.");
  const call = await response.json() as Call;
  if (!call.participants.some((p) => p.principalId === identity.principalId)) {
    throw new CallingHttpError(404, "NOT_FOUND", "Call not found.");
  }
  return call;
}

export function callingFailure(error: unknown) {
  const known = error instanceof CallingHttpError;
  return Response.json({ error: {
    code: known ? error.code : "SERVICE_UNAVAILABLE",
    message: known ? error.message : "Calling is temporarily unavailable.", requestId: null,
  } }, { status: known ? error.status : 503, headers: { "Cache-Control": "no-store" } });
}
