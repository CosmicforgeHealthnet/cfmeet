/**
 * Developer Portal HTTP client (Blueprint v1.0 Section 17, ORG-017).
 *
 * - Origin: the public API Gateway only (NEXT_PUBLIC_DEVELOPER_API_BASE_URL). Never
 *   Platform Access, the Developer Platform or a domain service directly, and never the
 *   legacy meeting-service `/api/v1/api-keys` API.
 * - Auth: the signed-in CF Meet session as `Authorization: Bearer <session>` (gateway
 *   auth mode DEVELOPER_SESSION). A machine credential (`cfmk_...`) is refused here: it
 *   must never be used or stored in a browser.
 * - Every operation below is a path of
 *   packages/contracts/platform-developer/http/v1.openapi.json with the same method and
 *   operationId (asserted by tests/developer-portal.test.mjs). Nothing else is called.
 *
 * Why hand-written and not `@cfmeet/platform-sdk`: the SDK (sdks/typescript) is a private,
 * unbuilt package (`dist/` is not committed) outside the Vercel root directory
 * (`apps/web`), so a `file:` dependency would need an SDK build step in both CI and
 * Vercel. This module covers only the console's session-bearer operations and is pinned
 * to the same contract by a test.
 *
 * Pure module: no React, no `@/` imports, erasable TypeScript only (node tests load it
 * with --experimental-strip-types).
 */

type Method = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

/** operationId -> [method, path template] (paths relative to the gateway origin). */
export const DEVELOPER_OPERATIONS = {
  listOrganizations: ["GET", "/v1/organizations"],
  createOrganization: ["POST", "/v1/organizations"],
  getOrganization: ["GET", "/v1/organizations/{organizationId}"],
  updateOrganization: ["PATCH", "/v1/organizations/{organizationId}"],
  closeOrganization: ["POST", "/v1/organizations/{organizationId}/close"],
  listMembers: ["GET", "/v1/organizations/{organizationId}/members"],
  changeMemberRole: ["PATCH", "/v1/organizations/{organizationId}/members/{memberId}"],
  removeMember: ["DELETE", "/v1/organizations/{organizationId}/members/{memberId}"],
  transferOwnership: ["POST", "/v1/organizations/{organizationId}/ownership-transfer"],
  listInvitations: ["GET", "/v1/organizations/{organizationId}/invitations"],
  createInvitation: ["POST", "/v1/organizations/{organizationId}/invitations"],
  revokeInvitation: ["POST", "/v1/organizations/{organizationId}/invitations/{invitationId}/revoke"],
  acceptInvitation: ["POST", "/v1/invitations/accept"],
  listProjects: ["GET", "/v1/organizations/{organizationId}/projects"],
  createProject: ["POST", "/v1/organizations/{organizationId}/projects"],
  getProject: ["GET", "/v1/projects/{projectId}"],
  updateProject: ["PATCH", "/v1/projects/{projectId}"],
  archiveProject: ["POST", "/v1/projects/{projectId}/archive"],
  suspendProject: ["POST", "/v1/projects/{projectId}/suspend"],
  resumeProject: ["POST", "/v1/projects/{projectId}/resume"],
  listProjectScopes: ["GET", "/v1/projects/{projectId}/scopes"],
  grantProjectScope: ["POST", "/v1/projects/{projectId}/scopes"],
  withdrawProjectScope: ["DELETE", "/v1/projects/{projectId}/scopes/{scope}"],
  listScopeRegistry: ["GET", "/v1/scopes"],
  listEnvironments: ["GET", "/v1/projects/{projectId}/environments"],
  createEnvironment: ["POST", "/v1/projects/{projectId}/environments"],
  getEnvironment: ["GET", "/v1/environments/{environmentId}"],
  suspendEnvironment: ["POST", "/v1/environments/{environmentId}/suspend"],
  resumeEnvironment: ["POST", "/v1/environments/{environmentId}/resume"],
  getEnvironmentEntitlements: ["GET", "/v1/environments/{environmentId}/entitlements"],
  getEnvironmentLimits: ["GET", "/v1/environments/{environmentId}/limits"],
  listServiceAccounts: ["GET", "/v1/environments/{environmentId}/service-accounts"],
  createServiceAccount: ["POST", "/v1/environments/{environmentId}/service-accounts"],
  getServiceAccount: ["GET", "/v1/environments/{environmentId}/service-accounts/{serviceAccountId}"],
  suspendServiceAccount: ["POST", "/v1/environments/{environmentId}/service-accounts/{serviceAccountId}/suspend"],
  resumeServiceAccount: ["POST", "/v1/environments/{environmentId}/service-accounts/{serviceAccountId}/resume"],
  putServiceAccountLimits: ["PUT", "/v1/environments/{environmentId}/service-accounts/{serviceAccountId}/limits"],
  listCredentials: ["GET", "/v1/environments/{environmentId}/service-accounts/{serviceAccountId}/credentials"],
  createCredential: ["POST", "/v1/environments/{environmentId}/service-accounts/{serviceAccountId}/credentials"],
  getCredential: ["GET", "/v1/environments/{environmentId}/credentials/{credentialId}"],
  rotateCredential: ["POST", "/v1/environments/{environmentId}/credentials/{credentialId}/rotate"],
  revokeCredential: ["POST", "/v1/environments/{environmentId}/credentials/{credentialId}/revoke"],
  suspendCredential: ["POST", "/v1/environments/{environmentId}/credentials/{credentialId}/suspend"],
  resumeCredential: ["POST", "/v1/environments/{environmentId}/credentials/{credentialId}/resume"],
  putCredentialLimits: ["PUT", "/v1/environments/{environmentId}/credentials/{credentialId}/limits"],
  getCredentialPolicy: ["GET", "/v1/organizations/{organizationId}/credential-policy"],
  putCredentialPolicy: ["PUT", "/v1/organizations/{organizationId}/credential-policy"],
  listEnvironmentWebhooks: ["GET", "/v1/environments/{environmentId}/webhooks"],
  createEnvironmentWebhook: ["POST", "/v1/environments/{environmentId}/webhooks"],
  getEnvironmentWebhook: ["GET", "/v1/environments/{environmentId}/webhooks/{webhookId}"],
  updateEnvironmentWebhook: ["PATCH", "/v1/environments/{environmentId}/webhooks/{webhookId}"],
  deleteEnvironmentWebhook: ["DELETE", "/v1/environments/{environmentId}/webhooks/{webhookId}"],
  rotateEnvironmentWebhookSecret: ["POST", "/v1/environments/{environmentId}/webhooks/{webhookId}/rotate-secret"],
  listEnvironmentWebhookDeliveries: ["GET", "/v1/environments/{environmentId}/webhooks/{webhookId}/deliveries"],
  listWebhookDeliveryAttempts: ["GET", "/v1/environments/{environmentId}/webhook-deliveries/{deliveryId}/attempts"],
  replayEnvironmentWebhookDelivery: ["POST", "/v1/environments/{environmentId}/webhook-deliveries/{deliveryId}/replay"],
  getEnvironmentUsage: ["GET", "/v1/environments/{environmentId}/usage"],
  listEnvironmentUsageFacts: ["GET", "/v1/environments/{environmentId}/usage/facts"],
  exportEnvironmentUsage: ["GET", "/v1/environments/{environmentId}/usage/export"],
  listEnvironmentUsageWarnings: ["GET", "/v1/environments/{environmentId}/usage/warnings"],
  getProductionAccess: ["GET", "/v1/projects/{projectId}/production-access"],
  listProductionAccessRequests: ["GET", "/v1/projects/{projectId}/production-access-requests"],
  submitProductionAccessRequest: ["POST", "/v1/projects/{projectId}/production-access-requests"],
  getProductionAccessRequest: ["GET", "/v1/production-access-requests/{requestId}"],
  cancelProductionAccessRequest: ["POST", "/v1/production-access-requests/{requestId}/cancel"],
  activateProductionAccess: ["POST", "/v1/production-access-requests/{requestId}/activate"],
  listActivity: ["GET", "/v1/organizations/{organizationId}/activity"],
  getOrganizationCommercial: ["GET", "/v1/organizations/{organizationId}/commercial"],
  listPlanAssignments: ["GET", "/v1/organizations/{organizationId}/plan-assignments"],
  putBillingAccount: ["PUT", "/v1/organizations/{organizationId}/billing-account"],
  listCommercialPlans: ["GET", "/v1/commercial-plans"],
  getSupportProfile: ["GET", "/v1/organizations/{organizationId}/support-profile"],
  putSupportProfile: ["PUT", "/v1/organizations/{organizationId}/support-profile"],
} as const satisfies Record<string, readonly [Method, string]>;

export type OperationId = keyof typeof DEVELOPER_OPERATIONS;

/** Public, unauthenticated gateway documents (API docs and SDK discovery). */
export const PUBLIC_DOCUMENTS = { discovery: "/discovery", openapi: "/openapi.json", docs: "/docs" } as const;

export type QueryValue = string | number | boolean | null | undefined;

export type RequestSpec = {
  operation: OperationId;
  path?: Record<string, string>;
  query?: Record<string, QueryValue>;
  body?: unknown;
  /** Sent as `Idempotency-Key` (creates, rotations). */
  idempotencyKey?: string;
  /** Default application/json; text/csv for the usage export. */
  accept?: string;
};

export type BuiltRequest = { method: Method; url: string; headers: Record<string, string>; body?: string };

const IDEMPOTENCY_KEY = /^[\x21-\x7e]{1,128}$/;

export class DeveloperApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId: string | null;
  readonly retryAfterSeconds: number | null;
  /** http: the gateway answered; network: unreachable; unavailable: not configured; session: no CF Meet session. */
  readonly kind: "http" | "network" | "unavailable" | "session";

  constructor(init: {
    status: number;
    code: string;
    message: string;
    requestId?: string | null;
    retryAfterSeconds?: number | null;
    kind?: "http" | "network" | "unavailable" | "session";
  }) {
    super(init.message);
    this.name = "DeveloperApiError";
    this.status = init.status;
    this.code = init.code;
    this.requestId = init.requestId ?? null;
    this.retryAfterSeconds = init.retryAfterSeconds ?? null;
    this.kind = init.kind ?? "http";
  }
}

/** Rejects anything that is not a CF Meet user session bearer (above all `cfmk_...`). */
export function assertSessionBearer(session: string | null | undefined): string {
  const value = (session ?? "").trim();
  if (!value) {
    throw new DeveloperApiError({
      status: 401,
      code: "AUTHENTICATION_REQUIRED",
      message: "Sign in to CF Meet to use the Developer Portal.",
      kind: "session",
    });
  }
  if (value.startsWith("cfmk_")) {
    throw new DeveloperApiError({
      status: 0,
      code: "MACHINE_CREDENTIAL_REFUSED",
      message: "A machine credential (cfmk_...) is never used in the browser. The Developer Portal uses your CF Meet session.",
      kind: "session",
    });
  }
  if (/[\s]/.test(value)) {
    throw new DeveloperApiError({ status: 401, code: "AUTHENTICATION_REQUIRED", message: "Invalid CF Meet session.", kind: "session" });
  }
  return value;
}

export function fillPath(template: string, params: Record<string, string> = {}): string {
  return template.replace(/\{([A-Za-z]+)\}/g, (_, name: string) => {
    const value = params[name];
    if (typeof value !== "string" || value.length === 0) {
      throw new Error(`missing path parameter ${name}`);
    }
    return encodeURIComponent(value);
  });
}

export function buildQuery(query: Record<string, QueryValue> = {}): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === "") continue;
    params.append(key, String(value));
  }
  const text = params.toString();
  return text ? `?${text}` : "";
}

/** Builds the gateway request for `spec` (pure: no network). */
export function buildRequest(baseUrl: string, spec: RequestSpec, session: string): BuiltRequest {
  const entry = DEVELOPER_OPERATIONS[spec.operation];
  if (!entry) throw new Error(`unknown Developer Platform operation ${String(spec.operation)}`);
  const [method, template] = entry;
  const headers: Record<string, string> = {
    Accept: spec.accept ?? "application/json",
    Authorization: `Bearer ${assertSessionBearer(session)}`,
  };
  let body: string | undefined;
  if (spec.body !== undefined) {
    if (method === "GET" || method === "DELETE") throw new Error(`${spec.operation} takes no request body`);
    headers["Content-Type"] = "application/json";
    body = JSON.stringify(spec.body);
  }
  if (spec.idempotencyKey !== undefined) {
    if (method !== "POST") throw new Error(`${spec.operation} does not take an Idempotency-Key`);
    if (!IDEMPOTENCY_KEY.test(spec.idempotencyKey)) throw new Error("invalid Idempotency-Key");
    headers["Idempotency-Key"] = spec.idempotencyKey;
  }
  const url = `${baseUrl.replace(/\/+$/, "")}${fillPath(template, spec.path)}${buildQuery(spec.query)}`;
  return body === undefined ? { method, url, headers } : { method, url, headers, body };
}

export function parseRetryAfter(value: string | null | undefined, now: number = Date.now()): number | null {
  if (!value) return null;
  const trimmed = value.trim();
  if (/^\d+$/.test(trimmed)) return Number(trimmed);
  const at = Date.parse(trimmed);
  if (Number.isNaN(at)) return null;
  return Math.max(0, Math.ceil((at - now) / 1000));
}

type HeaderLike = { get(name: string): string | null };

/** Maps a non-2xx gateway answer to a DeveloperApiError (`{"error":{code,message,requestId}}`). */
export function parseError(status: number, headers: HeaderLike, bodyText: string): DeveloperApiError {
  let code = `HTTP_${status}`;
  let message = `The Developer Platform answered ${status}.`;
  let requestId: string | null = headers.get("x-request-id");
  try {
    const parsed = JSON.parse(bodyText) as { error?: { code?: unknown; message?: unknown; requestId?: unknown } };
    const error = parsed?.error;
    if (error && typeof error === "object") {
      if (typeof error.code === "string" && error.code) code = error.code;
      if (typeof error.message === "string" && error.message) message = error.message;
      if (typeof error.requestId === "string" && error.requestId) requestId = error.requestId;
    }
  } catch {
    // Not a contract error body (an edge proxy page, an empty body): keep the defaults.
  }
  return new DeveloperApiError({
    status,
    code,
    message,
    requestId,
    retryAfterSeconds: parseRetryAfter(headers.get("retry-after")),
  });
}

export type DeveloperResult<T> = { data: T; requestId: string | null; replayed: boolean };

export type DeveloperClientOptions = {
  /** The gateway origin, or null when the Developer Platform is not configured here. */
  baseUrl: string | null;
  /** The current CF Meet session bearer (null when signed out / expired). */
  getSession: () => string | null | undefined;
  /** Refreshes the CF Meet session through the existing sign-in flow; returns the new bearer. */
  refreshSession?: () => Promise<string | null | undefined>;
  fetchImpl?: typeof fetch;
};

export const UNAVAILABLE_MESSAGE =
  "The Developer Platform is not available in this environment. The API Gateway and Developer Platform have not been deployed here (PLT-005).";

export function createDeveloperClient(options: DeveloperClientOptions) {
  const doFetch = options.fetchImpl ?? ((input: RequestInfo | URL, init?: RequestInit) => fetch(input, init));

  async function session(forceRefresh: boolean): Promise<string> {
    let token = forceRefresh ? null : options.getSession();
    if (!token && options.refreshSession) token = await options.refreshSession().catch(() => null);
    return assertSessionBearer(token);
  }

  async function send(spec: RequestSpec): Promise<Response> {
    if (!options.baseUrl) {
      throw new DeveloperApiError({ status: 0, code: "DEVELOPER_PLATFORM_UNAVAILABLE", message: UNAVAILABLE_MESSAGE, kind: "unavailable" });
    }
    const baseUrl = options.baseUrl;
    const attempt = async (token: string) => {
      const built = buildRequest(baseUrl, spec, token);
      try {
        return await doFetch(built.url, {
          method: built.method,
          headers: built.headers,
          body: built.body,
          // Bearer only: no cookies to the gateway, no cached secret-bearing answers,
          // no redirect that could carry the session elsewhere.
          credentials: "omit",
          cache: "no-store",
          redirect: "error",
          referrerPolicy: "no-referrer",
        });
      } catch {
        throw new DeveloperApiError({
          status: 0,
          code: "DEVELOPER_PLATFORM_UNREACHABLE",
          message: "The Developer Platform could not be reached from this browser.",
          kind: "network",
        });
      }
    };
    let response = await attempt(await session(false));
    if (response.status === 401 && options.refreshSession) {
      response = await attempt(await session(true));
    }
    if (!response.ok) {
      throw parseError(response.status, response.headers, await response.text().catch(() => ""));
    }
    return response;
  }

  async function request<T>(spec: RequestSpec): Promise<DeveloperResult<T>> {
    const response = await send(spec);
    const requestId = response.headers.get("x-request-id");
    const replayed = response.headers.get("idempotent-replayed") === "true";
    if (response.status === 204) return { data: undefined as T, requestId, replayed };
    const text = await response.text();
    return { data: (text ? JSON.parse(text) : undefined) as T, requestId, replayed };
  }

  return {
    configured: Boolean(options.baseUrl),
    request,
    call: async <T>(spec: RequestSpec): Promise<T> => (await request<T>(spec)).data,
    /** Raw body download (usage export); the caller saves it as a file. */
    download: async (spec: RequestSpec): Promise<{ blob: Blob; requestId: string | null }> => {
      const response = await send(spec);
      return { blob: await response.blob(), requestId: response.headers.get("x-request-id") };
    },
    /** Public, unauthenticated gateway discovery (no session is sent). */
    discovery: async <T>(): Promise<T> => {
      if (!options.baseUrl) {
        throw new DeveloperApiError({ status: 0, code: "DEVELOPER_PLATFORM_UNAVAILABLE", message: UNAVAILABLE_MESSAGE, kind: "unavailable" });
      }
      let response: Response;
      try {
        response = await doFetch(`${options.baseUrl.replace(/\/+$/, "")}${PUBLIC_DOCUMENTS.discovery}`, {
          headers: { Accept: "application/json" },
          credentials: "omit",
          cache: "no-store",
          redirect: "error",
        });
      } catch {
        throw new DeveloperApiError({
          status: 0,
          code: "DEVELOPER_PLATFORM_UNREACHABLE",
          message: "The Developer Platform could not be reached from this browser.",
          kind: "network",
        });
      }
      if (!response.ok) throw parseError(response.status, response.headers, await response.text().catch(() => ""));
      return (await response.json()) as T;
    },
  };
}

export type DeveloperClient = ReturnType<typeof createDeveloperClient>;

/** A fresh Idempotency-Key for one user-initiated create (reused when that submission is retried). */
export function newIdempotencyKey(): string {
  const c = (globalThis as { crypto?: Crypto }).crypto;
  if (c && typeof c.randomUUID === "function") return `cfmeet-console-${c.randomUUID()}`;
  const bytes = new Uint8Array(16);
  if (c && typeof c.getRandomValues === "function") c.getRandomValues(bytes);
  else for (let i = 0; i < bytes.length; i++) bytes[i] = Math.floor(Math.random() * 256);
  return `cfmeet-console-${Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")}`;
}
