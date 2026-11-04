import { callingFailure, callingFetch, callingIdentity, CallingHttpError, ownCall, ownEndpoints } from "@/lib/calling/server";

export const runtime = "nodejs";
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
type Context = { params: Promise<{ path: string[] }> };

const resourceId = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
const enterpriseRules: Array<[RegExp, string[]]> = [
  [/^v1\/calling\/(usage|limits)$/, ["GET"]],
  [new RegExp(`^v1/enterprise/(policies|departments|extensions|ring-groups|queues|ivr-flows|business-hours|public-contacts|forwarding-rules|voicemail/mailboxes|recording-policies|sip-devices)$`), ["GET", "POST"]],
  [new RegExp(`^v1/enterprise/(policies|departments|extensions|ring-groups|queues|ivr-flows|forwarding-rules|voicemail/mailboxes|recording-policies)/${resourceId}$`), ["GET", "PATCH", "DELETE"]],
  [new RegExp(`^v1/enterprise/business-hours/${resourceId}$`), ["GET", "PUT", "DELETE"]],
  [new RegExp(`^v1/enterprise/public-contacts/${resourceId}$`), ["GET", "DELETE"]],
  [new RegExp(`^v1/enterprise/sip-devices/${resourceId}$`), ["GET", "DELETE"]],
  [new RegExp(`^v1/enterprise/departments/${resourceId}/members$`), ["PUT"]],
  [new RegExp(`^v1/enterprise/ring-groups/${resourceId}/members$`), ["PUT"]],
  [new RegExp(`^v1/enterprise/queues/${resourceId}/members$`), ["PUT"]],
  [new RegExp(`^v1/enterprise/queues/${resourceId}/entries$`), ["GET"]],
  [new RegExp(`^v1/enterprise/ivr-flows/${resourceId}/versions$`), ["GET", "POST"]],
  [new RegExp(`^v1/enterprise/ivr-flows/${resourceId}/versions/[0-9]+$`), ["GET"]],
  [new RegExp(`^v1/enterprise/ivr-flows/${resourceId}/activation$`), ["PUT"]],
  [new RegExp(`^v1/enterprise/business-hours/${resourceId}/evaluation$`), ["POST"]],
  [/^v1\/enterprise\/audit$/, ["GET"]],
  [/^v1\/enterprise\/effective-policy$/, ["GET"]],
  [new RegExp(`^v1/enterprise/voicemail/messages$`), ["GET"]],
  [new RegExp(`^v1/enterprise/voicemail/messages/${resourceId}$`), ["GET", "DELETE"]],
  [new RegExp(`^v1/enterprise/voicemail/messages/${resourceId}/read-state$`), ["PUT"]],
  [new RegExp(`^v1/enterprise/voicemail/messages/${resourceId}/media-access$`), ["POST"]],
  [new RegExp(`^v1/enterprise/recordings/${resourceId}$`), ["GET", "DELETE"]],
  [new RegExp(`^v1/enterprise/recordings/${resourceId}/media-access$`), ["POST"]],
  [new RegExp(`^v1/enterprise/recordings/${resourceId}/access-audit$`), ["GET"]],
  [/^v1\/webhooks$/, ["GET", "POST"]],
  [new RegExp(`^v1/webhooks/${resourceId}$`), ["GET", "PATCH", "DELETE"]],
  [new RegExp(`^v1/webhooks/${resourceId}/rotate-secret$`), ["POST"]],
  [new RegExp(`^v1/webhooks/${resourceId}/deliveries$`), ["GET"]],
  [new RegExp(`^v1/webhook-deliveries/${resourceId}/replay$`), ["POST"]],
];

const privacyRelayRules: Array<[RegExp, string[]]> = [
  [/^v1\/privacy-relay\/contexts$/, ["GET", "POST"]],
  [new RegExp(`^v1/privacy-relay/contexts/${resourceId}$`), ["GET", "PATCH"]],
  [new RegExp(`^v1/privacy-relay/contexts/${resourceId}/evidence$`), ["GET"]],
  [new RegExp(`^v1/privacy-relay/contexts/${resourceId}/sessions$`), ["POST"]],
  [new RegExp(`^v1/privacy-relay/contexts/${resourceId}/(suspend|resume|complete|revoke)$`), ["POST"]],
  [/^v1\/privacy-relay\/sessions$/, ["GET"]],
  [new RegExp(`^v1/privacy-relay/sessions/${resourceId}$`), ["GET"]],
  [new RegExp(`^v1/privacy-relay/sessions/${resourceId}/evidence$`), ["GET"]],
  [new RegExp(`^v1/privacy-relay/sessions/${resourceId}/(suspend|resume|revoke)$`), ["POST"]],
  [/^v1\/privacy-relay\/contact-access-grants$/, ["POST"]],
  [/^v1\/privacy-relay\/contacts$/, ["POST"]],
  [new RegExp(`^v1/privacy-relay/contacts/${resourceId}$`), ["GET", "PATCH"]],
  [new RegExp(`^v1/privacy-relay/contacts/${resourceId}/disable$`), ["POST"]],
];

function isAllowedResourceRoute(route: string, method: string): boolean {
  return [...enterpriseRules, ...privacyRelayRules].some(
    ([pattern, methods]) => pattern.test(route) && methods.includes(method),
  );
}

function requireResourceAdministrator(principalId: string, route: string) {
  const variable = route.startsWith("v1/privacy-relay/")
    ? "CALLING_PRIVACY_ADMIN_PRINCIPAL_IDS"
    : "CALLING_ADMIN_PRINCIPAL_IDS";
  const allowedPrincipals = new Set(
    (process.env[variable] ?? "").split(",").map((id) => id.trim()).filter(Boolean),
  );
  if (!allowedPrincipals.has(principalId)) {
    throw new CallingHttpError(403, "INSUFFICIENT_SCOPE", "This Calling administration surface is not enabled for your account.");
  }
}

async function requireOwnBlockRule(blockId: string, principalId: string) {
  const response = await callingFetch(`/v1/blocks?principalId=${encodeURIComponent(principalId)}`);
  if (!response.ok) {
    throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED", "Could not verify block ownership.");
  }
  const payload: unknown = await response.json();
  if (!payload || typeof payload !== "object" || !("items" in payload) || !Array.isArray(payload.items)) {
    throw new CallingHttpError(502, "UPSTREAM_ERROR", "Calling returned an invalid block list.");
  }
  const ownsRule = payload.items.some((item) =>
    !!item && typeof item === "object" && "blockId" in item &&
    item.blockId === blockId && "principalId" in item && item.principalId === principalId,
  );
  if (!ownsRule) throw new CallingHttpError(404, "NOT_FOUND", "Block rule not found.");
}

/** Check if calling dev mode is enabled */
function isDevMode(): boolean {
  return process.env.CALLING_DEV_MODE === "true";
}

/** Generate mock responses for dev mode when calling service is unavailable */
function devModeResponse(route: string, method: string, identity: { principalId: string }): Response | null {
  const json = (data: unknown) => Response.json(data, { headers: { "Cache-Control": "no-store" } });

  // Endpoints - empty list in dev mode
  if (route === "v1/calling/endpoints" && method === "GET") {
    return json({ data: [], pagination: { cursor: null, hasMore: false } });
  }

  // Calls - empty history in dev mode
  if (route === "v1/calls" && method === "GET") {
    return json({ data: [], pagination: { cursor: null, hasMore: false } });
  }

  // Blocks - empty list in dev mode
  if (route === "v1/blocks" && method === "GET") {
    return json({ items: [] });
  }

  // Forwarding rules - empty list in dev mode
  if (route === "v1/enterprise/forwarding-rules" && method === "GET") {
    return json({ data: [], pagination: { cursor: null, hasMore: false } });
  }

  // Voicemail mailboxes - empty list in dev mode
  if (route === "v1/enterprise/voicemail/mailboxes" && method === "GET") {
    return json({ data: [], pagination: { cursor: null, hasMore: false } });
  }

  // Calling limits - mock data in dev mode
  if (route === "v1/calling/limits" && method === "GET") {
    return json({
      data: [
        { limitId: "dev-limit-1", name: "Monthly Minutes", unit: "MINUTES", quota: 1000, consumed: 0, status: "OK", periodStart: new Date().toISOString(), periodEnd: new Date(Date.now() + 30 * 24 * 60 * 60 * 1000).toISOString() },
      ],
      pagination: { cursor: null, hasMore: false },
    });
  }

  // Calling usage - empty data in dev mode
  if (route === "v1/calling/usage" && method === "GET") {
    return json({
      data: [],
      aggregation: { totalMinutes: 0, totalCalls: 0, totalCost: 0 },
      pagination: { cursor: null, hasMore: false },
    });
  }

  return null;
}

async function handle(request: Request, context: Context) {
  try {
    const identity = await callingIdentity(request);
    const { path } = await context.params;
    if (request.method === "GET" && path.join("/") === "identity") {
      return Response.json(identity, { headers: { "Cache-Control": "no-store" } });
    }

    // Dev mode: return mock responses without calling the backend
    if (isDevMode()) {
      const route = path.join("/");
      const mockResponse = devModeResponse(route, request.method, identity);
      if (mockResponse) return mockResponse;
    }
    const method = request.method;
    const url = new URL(request.url);
    let body: Record<string, unknown> | undefined;
    if (["POST", "PATCH", "PUT"].includes(method)) {
      const text = await request.text();
      if (text.length > 16_384) throw new CallingHttpError(413, "VALIDATION_ERROR", "Request too large.");
      try { body = JSON.parse(text || "{}"); } catch { throw new CallingHttpError(400, "VALIDATION_ERROR", "Invalid JSON."); }
      if (!body || Array.isArray(body) || typeof body !== "object") throw new CallingHttpError(400, "VALIDATION_ERROR", "Expected a JSON object.");
    } else if (method === "DELETE") {
      const text = await request.text();
      if (text.length > 16_384) throw new CallingHttpError(413, "VALIDATION_ERROR", "Request too large.");
      if (text) {
        try { body = JSON.parse(text); } catch { throw new CallingHttpError(400, "VALIDATION_ERROR", "Invalid JSON."); }
        if (!body || Array.isArray(body) || typeof body !== "object") throw new CallingHttpError(400, "VALIDATION_ERROR", "Expected a JSON object.");
      }
    }
    const route = path.join("/");
    let allowed = false;
    const requireOwnEndpoint = async (id: unknown) => {
      const endpoints = await ownEndpoints(identity);
      if (!endpoints.data.some((e) => e.endpointId === id && e.status === "ACTIVE")) {
        throw new CallingHttpError(404, "NOT_FOUND", "Device not found.");
      }
    };
    if (route === "v1/blocks") {
      allowed = method === "GET" || method === "POST";
      if (method === "GET") url.searchParams.set("principalId", identity.principalId);
      if (body) body.principalId = identity.principalId;
    } else if (path.length === 3 && path[0] === "v1" && path[1] === "blocks" && uuid.test(path[2])) {
      allowed = method === "DELETE";
      if (allowed) await requireOwnBlockRule(path[2], identity.principalId);
    } else if (route === "v1/calling/endpoints") {
      allowed = method === "GET" || method === "POST";
      if (method === "GET") url.searchParams.set("principalId", identity.principalId);
      if (body) body.principalId = identity.principalId;
    } else if (path.length === 4 && path.slice(0, 3).join("/") === "v1/calling/endpoints" && uuid.test(path[3])) {
      allowed = method === "DELETE";
      await requireOwnEndpoint(path[3]);
    } else if (route === "v1/calling/signaling-credentials" && body) {
      allowed = method === "POST";
      body.principalId = identity.principalId;
      await requireOwnEndpoint(body.endpointId);
    } else if (route === "v1/calls") {
      allowed = method === "GET" || method === "POST";
      if (method === "GET") url.searchParams.set("principalId", identity.principalId);
      if (body) {
        body.initiatorPrincipalId = identity.principalId;
        // Contact/relay administration needs a separate end-user authorization
        // boundary. The native UI exposes app calls and extensions only.
        if (body.callbackRequested || body.targetContactReference || body.initiatorContactReference || body.presentedNumberResourceId || body.privacyRelaySessionId) {
          throw new CallingHttpError(403, "INSUFFICIENT_SCOPE", "Protected-contact calling is not enabled for this client.");
        }
        await requireOwnEndpoint(body.initiatorEndpointId);
      }
    } else if (path[0] === "v1" && path[1] === "calls" && uuid.test(path[2] || "")) {
      const call = await ownCall(identity, path[2]);
      // Historical participation still proves ownership for idempotent retries
      // after termination. The backend enforces whether a new command is legal.
      const ownParticipant = call.participants.find((p) => p.principalId === identity.principalId);
      if (path.length === 3) allowed = method === "GET";
      if (path.length === 4 && path[3] === "actions") allowed = method === "GET";
      if (path.length === 4 && ["cancel", "end", "hold", "resume", "transfer", "accept", "decline"].includes(path[3]) && body) {
        allowed = method === "POST" && !!ownParticipant;
        if (["cancel", "end", "hold", "resume"].includes(path[3]) && call.initiatorPrincipalId !== identity.principalId) allowed = false;
        if (path[3] === "transfer") body.transferringParticipantId = ownParticipant?.participantId;
        if (["accept", "decline"].includes(path[3])) {
          await requireOwnEndpoint(body.endpointId);
          if (!call.invitations.some((i) => i.invitationId === body?.invitationId && i.endpointId === body?.endpointId && i.participantId === ownParticipant?.participantId)) allowed = false;
        }
      }
      if (path.length === 5 && path[3] === "ivr" && path[4] === "input" && body) {
        allowed = method === "POST" && call.initiatorPrincipalId === identity.principalId;
        await requireOwnEndpoint(body.endpointId);
      }
      if (path.length === 6 && path[3] === "sessions" && uuid.test(path[4]) && ["media-credentials", "remove"].includes(path[5])) {
        const session = call.sessions.find((s) => s.sessionId === path[4] && s.participantId === ownParticipant?.participantId);
        allowed = method === "POST" && !!session;
      }
    } else if (isAllowedResourceRoute(route, method)) {
      requireResourceAdministrator(identity.principalId, route);
      allowed = true;
    }
    if (!allowed) throw new CallingHttpError(404, "NOT_FOUND", "Calling operation not found.");
    const headers = new Headers();
    const key = request.headers.get("Idempotency-Key");
    if (key) headers.set("Idempotency-Key", key);
    const upstream = await callingFetch(`/${route}${url.search}`, {
      method, headers, body: body ? JSON.stringify(body) : undefined,
    });
    if (upstream.status === 401) {
      throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED", "The calling connection is not available for this account.");
    }
    const responseHeaders = new Headers({ "Content-Type": "application/json", "Cache-Control": "no-store" });
    for (const name of ["Idempotent-Replayed", "Retry-After", "X-Request-Id"]) {
      const value = upstream.headers.get(name);
      if (value) responseHeaders.set(name, value);
    }
    return new Response(upstream.body, { status: upstream.status, headers: responseHeaders });
  } catch (error) { return callingFailure(error); }
}

export { handle as GET, handle as POST, handle as PATCH, handle as PUT, handle as DELETE };
