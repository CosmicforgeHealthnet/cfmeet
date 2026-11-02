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

/**
 * Check if calling dev mode is enabled.
 * In dev mode, we bypass Platform Access and return a mock identity.
 */
function isDevMode(): boolean {
  return process.env.CALLING_DEV_MODE === "true";
}

/**
 * Get calling identity for the current request.
 * In dev mode, returns a mock identity for testing.
 * In production, verifies identity through the messaging service.
 */
export async function callingIdentity(request: Request): Promise<CallingIdentity> {
  const authorization = request.headers.get("authorization");
  if (!authorization?.startsWith("Bearer ")) {
    throw new CallingHttpError(401, "AUTHENTICATION_FAILED", "Sign in to use calling.");
  }

  // Dev mode: return mock identity for testing without full backend
  if (isDevMode()) {
    const devPrincipalId = process.env.CALLING_DEV_PRINCIPAL_ID || "dev-user-001";
    return {
      principalId: devPrincipalId,
      tenantId: "dev-tenant",
      applicationId: "dev-application",
      environmentId: "dev-environment",
    };
  }

  // Production mode: verify through messaging service
  const base = process.env.NEXT_PUBLIC_API_URL || "http://localhost:8080/api/v1";
  let response: Response;
  try {
    response = await fetch(`${base.replace(/\/api\/v1\/?$/, "/api/v2")}/messaging/me`, {
      headers: { Authorization: authorization }, cache: "no-store", redirect: "error",
      signal: AbortSignal.timeout(15_000),
    });
  } catch (error) {
    // Network error or timeout
    throw new CallingHttpError(503, "SERVICE_UNAVAILABLE",
      "Cannot reach the identity service. Ensure the meeting service is running.");
  }

  if (!response.ok) {
    if (response.status === 401) {
      throw new CallingHttpError(401, "AUTHENTICATION_FAILED", "Sign in to use calling.");
    }
    throw new CallingHttpError(503, "AUTHENTICATION_FAILED",
      "Your communication identity is unavailable. Check that the meeting service is running.");
  }

  const identity = await response.json() as CallingIdentity;

  // Validate calling configuration
  if (!identity.principalId) {
    throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED",
      "No principal ID returned from identity service.");
  }

  if (!process.env.CALLING_PLATFORM_CREDENTIAL) {
    throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED",
      "CALLING_PLATFORM_CREDENTIAL is not set. Set CALLING_DEV_MODE=true for development.");
  }

  if (identity.tenantId !== process.env.CALLING_TENANT_ID ||
      identity.applicationId !== process.env.CALLING_APPLICATION_ID ||
      identity.environmentId !== process.env.CALLING_ENVIRONMENT_ID) {
    throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED",
      "Calling credential scope does not match your identity. Check CALLING_TENANT_ID, CALLING_APPLICATION_ID, CALLING_ENVIRONMENT_ID.");
  }

  return identity;
}

export async function callingFetch(path: string, init: RequestInit = {}) {
  const base = process.env.CALLING_API_URL || process.env.NEXT_PUBLIC_CALLING_API_URL || "http://localhost:8090";
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
