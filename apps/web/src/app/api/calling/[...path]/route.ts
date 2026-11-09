import { callingDevMode, callingFailure, callingFetch, callingIdentity, CallingHttpError, ownCall, ownEndpoints, type CallingIdentity } from "@/lib/calling/server";
import { mayControl, ownParticipant, activeParticipant, type ControlOperation } from "@/lib/calling/call-permissions";
import type { Call } from "@/types/calling";

/**
 * Same-origin Calling adapter (personal scope, shared platform credential).
 *
 * Every operation is an explicit allowlist entry of the served Calling contract
 * (packages/contracts/calling/http/v1.openapi.json). The acting principal is never taken from
 * the request: it is injected from the verified login (principalId / initiatorPrincipalId /
 * transferringParticipantId) and ownership is checked before anything reaches Calling.
 *
 *  - Call control follows the party rules of ADR 0009 (CAL-002, `call-permissions.ts`).
 *  - Recording consent / controls and abuse reports are participant-bound (CAL-005).
 *  - Voicemail and forwarding are self-service, bound to the signed-in principal (CAL-006);
 *    the backend's `VoicemailRequester::Principal` (principalId) gives the owner view only.
 *  - The remaining enterprise administration of the SHARED scope stays behind the
 *    CALLING_ADMIN_PRINCIPAL_IDS allowlist until it is retired (CAL-007); organization
 *    administration goes through Meeting's `/api/v2/organizations/{id}/calling/{path}`.
 */

export const runtime = "nodejs";
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
type Context = { params: Promise<{ path: string[] }> };

const resourceId = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
/** Shared-scope enterprise administration (legacy allowlist; personal self-service routes are handled separately). */
const enterpriseRules: Array<[RegExp, string[]]> = [
  [/^v1\/calling\/(usage|limits)$/, ["GET"]],
  [new RegExp(`^v1/enterprise/(policies|departments|extensions|ring-groups|queues|ivr-flows|business-hours|public-contacts|recording-policies|sip-devices)$`), ["GET", "POST"]],
  [/^v1\/enterprise\/voicemail\/mailboxes$/, ["POST"]],
  [new RegExp(`^v1/enterprise/(policies|departments|extensions|ring-groups|queues|ivr-flows|voicemail/mailboxes|recording-policies)/${resourceId}$`), ["GET", "PATCH", "DELETE"]],
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
  [/^v1\/enterprise\/recordings$/, ["GET"]],
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

/** Abuse categories of `services/calling/src/calls/app/abuse_reports.rs` (AbuseCategory). */
const ABUSE_CATEGORIES = new Set(["UNWANTED", "HARASSMENT", "SPAM", "FRAUD_SUSPECTED", "OTHER"]);
/** ConsentDecision of the enterprise contract. */
const CONSENT_DECISIONS = new Set(["GRANTED", "DECLINED"]);
/** Destinations an end user may forward their own calls to; queues / IVR / ring groups stay administrative. */
const SELF_FORWARDING_DESTINATIONS = new Set(["PRINCIPAL", "EXTENSION", "VOICEMAIL"]);
/** Upper bounds of the self-service mailbox discovery (pages × 100 mailboxes, ownership probes). */
const MAILBOX_PAGES = 5;
const MAILBOX_PROBES = 50;

type Body = Record<string, unknown>;
/** `true`: forward as received (after in-place binding); `{ body }`: forward this body instead; Response: answer directly; `false`: 404. */
type Decision = boolean | { body: Body | undefined } | Response;

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

async function upstreamJson(path: string): Promise<{ status: number; body: unknown }> {
  const response = await callingFetch(path);
  if (response.status === 401) throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED", "The calling connection is not available for this account.");
  let body: unknown = null;
  try { body = await response.json(); } catch { body = null; }
  return { status: response.status, body };
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

/** A forwarding rule is the user's own only when Calling says it belongs to their principal. */
async function requireOwnForwardingRule(ruleId: string, principalId: string) {
  const { status, body } = await upstreamJson(`/v1/enterprise/forwarding-rules/${ruleId}`);
  if (status === 404) throw new CallingHttpError(404, "NOT_FOUND", "Forwarding rule not found.");
  if (status !== 200 || !body || typeof body !== "object") {
    throw new CallingHttpError(503, "SERVICE_UNAVAILABLE", "Could not verify forwarding rule ownership.");
  }
  if ((body as { principalId?: unknown }).principalId !== principalId) {
    throw new CallingHttpError(404, "NOT_FOUND", "Forwarding rule not found.");
  }
}

type MailboxView = { mailboxId: string; owner?: { type?: string; id?: string } };

/** Does `principalId` own this mailbox? Calling decides (EXTENSION / QUEUE / RING_GROUP owners resolve server-side). */
async function ownsMailbox(mailbox: MailboxView, principalId: string): Promise<boolean> {
  if (mailbox.owner?.type === "PRINCIPAL") return mailbox.owner.id === principalId;
  const probe = await callingFetch(`/v1/enterprise/voicemail/messages?mailboxId=${encodeURIComponent(mailbox.mailboxId)}&principalId=${encodeURIComponent(principalId)}&limit=1`);
  if (probe.status === 200) return true;
  if (probe.status === 404 || probe.status === 400) return false;
  throw new CallingHttpError(503, "SERVICE_UNAVAILABLE", "Could not verify your voicemail mailboxes.");
}

/**
 * "My mailboxes": the scope's mailboxes filtered to those Calling lets this principal read.
 * Calling has no owner filter on GET /v1/enterprise/voicemail/mailboxes (recorded gap), so the
 * adapter pages through (bounded) and probes non-PRINCIPAL owners with the owner view.
 */
async function ownMailboxes(principalId: string): Promise<Response> {
  const owned: MailboxView[] = [];
  let cursor: string | null = null;
  let probes = 0;
  let truncated = false;
  for (let page = 0; page < MAILBOX_PAGES; page += 1) {
    const query: string = `limit=100&status=ACTIVE${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`;
    const { status, body } = await upstreamJson(`/v1/enterprise/voicemail/mailboxes?${query}`);
    if (status === 403) {
      // The deployment's credential holds no voicemail scope: an honest empty answer.
      return Response.json({ mailboxes: [], nextCursor: null, truncated: false }, { headers: { "Cache-Control": "no-store" } });
    }
    if (status !== 200 || !body || typeof body !== "object" || !Array.isArray((body as { mailboxes?: unknown }).mailboxes)) {
      throw new CallingHttpError(503, "SERVICE_UNAVAILABLE", "Voicemail is temporarily unavailable.");
    }
    const list = body as { mailboxes: MailboxView[]; nextCursor: string | null };
    for (const mailbox of list.mailboxes) {
      if (mailbox.owner?.type !== "PRINCIPAL") {
        if (probes >= MAILBOX_PROBES) { truncated = true; continue; }
        probes += 1;
      }
      if (await ownsMailbox(mailbox, principalId)) owned.push(mailbox);
    }
    cursor = list.nextCursor;
    if (!cursor) break;
    if (page === MAILBOX_PAGES - 1) truncated = true;
  }
  return Response.json({ mailboxes: owned, nextCursor: null, truncated }, { headers: { "Cache-Control": "no-store" } });
}

/** Mock reads for dev mode (development builds without a credential only — see runtime-guard.ts). */
function devModeResponse(route: string, method: string): Response | null {
  const json = (data: unknown) => Response.json(data, { headers: { "Cache-Control": "no-store" } });
  if (method !== "GET") return null;
  if (route === "v1/calling/endpoints") return json({ data: [] });
  if (route === "v1/calls") return json({ data: [], nextCursor: null });
  if (route === "v1/blocks") return json({ items: [] });
  if (route === "v1/abuse-reports") return json({ items: [] });
  if (route === "v1/enterprise/forwarding-rules") return json({ forwardingRules: [], nextCursor: null });
  if (route === "v1/enterprise/voicemail/mailboxes") return json({ mailboxes: [], nextCursor: null, truncated: false });
  return null;
}

function parseBody(text: string): Body {
  if (text.length > 16_384) throw new CallingHttpError(413, "VALIDATION_ERROR", "Request too large.");
  let body: unknown;
  try { body = JSON.parse(text || "{}"); } catch { throw new CallingHttpError(400, "VALIDATION_ERROR", "Invalid JSON."); }
  if (!body || Array.isArray(body) || typeof body !== "object") throw new CallingHttpError(400, "VALIDATION_ERROR", "Expected a JSON object.");
  return body as Body;
}

async function handle(request: Request, context: Context) {
  try {
    const identity = await callingIdentity(request);
    const { path } = await context.params;
    if (request.method === "GET" && path.join("/") === "identity") {
      return Response.json({ principalId: identity.principalId }, { headers: { "Cache-Control": "no-store" } });
    }

    if (callingDevMode()) {
      const mockResponse = devModeResponse(path.join("/"), request.method);
      if (mockResponse) return mockResponse;
    }
    const method = request.method;
    const url = new URL(request.url);
    let body: Body | undefined;
    if (["POST", "PATCH", "PUT"].includes(method)) {
      body = parseBody(await request.text());
    } else if (method === "DELETE") {
      const text = await request.text();
      if (text) body = parseBody(text);
    }
    const route = path.join("/");
    const decision = await authorize({ identity, route, path, method, url, body });
    if (decision instanceof Response) return decision;
    if (!decision) throw new CallingHttpError(404, "NOT_FOUND", "Calling operation not found.");
    if (decision !== true) body = decision.body;
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

type Authorization = { identity: CallingIdentity; route: string; path: string[]; method: string; url: URL; body: Body | undefined };

async function authorize({ identity, route, path, method, url, body }: Authorization): Promise<Decision> {
  const principalId = identity.principalId;
  const requireOwnEndpoint = async (id: unknown) => {
    const endpoints = await ownEndpoints(identity);
    if (!endpoints.data.some((e) => e.endpointId === id && e.status === "ACTIVE")) {
      throw new CallingHttpError(404, "NOT_FOUND", "Device not found.");
    }
  };

  if (route === "v1/blocks") {
    if (method === "GET") url.searchParams.set("principalId", principalId);
    if (body) body.principalId = principalId;
    return method === "GET" || method === "POST";
  }
  if (path.length === 3 && path[0] === "v1" && path[1] === "blocks" && uuid.test(path[2])) {
    if (method !== "DELETE") return false;
    await requireOwnBlockRule(path[2], principalId);
    return true;
  }
  if (route === "v1/calling/endpoints") {
    if (method === "GET") url.searchParams.set("principalId", principalId);
    if (body) body.principalId = principalId;
    return method === "GET" || method === "POST";
  }
  if (path.length === 4 && path.slice(0, 3).join("/") === "v1/calling/endpoints" && uuid.test(path[3])) {
    if (method !== "DELETE") return false;
    await requireOwnEndpoint(path[3]);
    return true;
  }
  if (route === "v1/calling/signaling-credentials" && body) {
    if (method !== "POST") return false;
    body.principalId = principalId;
    await requireOwnEndpoint(body.endpointId);
    return true;
  }
  if (route === "v1/abuse-reports") {
    if (method !== "GET") return false;
    url.search = `?principalId=${encodeURIComponent(principalId)}`;
    return true;
  }
  if (route === "v1/calls") {
    if (method === "GET") url.searchParams.set("principalId", principalId);
    if (body) {
      body.initiatorPrincipalId = principalId;
      // Contact/relay administration needs a separate end-user authorization
      // boundary. The native UI exposes app calls (1:1 and group) and extensions only.
      if (body.callbackRequested || body.targetContactReference || body.initiatorContactReference || body.presentedNumberResourceId || body.privacyRelaySessionId) {
        throw new CallingHttpError(403, "INSUFFICIENT_SCOPE", "Protected-contact calling is not enabled for this client.");
      }
      await requireOwnEndpoint(body.initiatorEndpointId);
    }
    return method === "GET" || method === "POST";
  }
  if (path[0] === "v1" && path[1] === "calls" && uuid.test(path[2] || "")) {
    return authorizeCallRoute(identity, path, method, url, body, requireOwnEndpoint);
  }

  // ---- Self-service voicemail (CAL-006): always the owner view of the signed-in principal ----
  if (route === "v1/enterprise/voicemail/mailboxes" && method === "GET") {
    return ownMailboxes(principalId);
  }
  if (route === "v1/enterprise/voicemail/messages" && method === "GET") {
    if (!uuid.test(url.searchParams.get("mailboxId") ?? "")) throw new CallingHttpError(400, "VALIDATION_ERROR", "mailboxId is required.");
    url.searchParams.set("principalId", principalId);
    return true;
  }
  if (path.length === 5 && path.slice(0, 4).join("/") === "v1/enterprise/voicemail/messages" && uuid.test(path[4])) {
    if (method !== "GET" && method !== "DELETE") return false;
    url.searchParams.set("principalId", principalId);
    return { body: undefined };
  }
  if (path.length === 6 && path.slice(0, 4).join("/") === "v1/enterprise/voicemail/messages" && uuid.test(path[4])) {
    if (path[5] === "read-state" && method === "PUT" && body) {
      return { body: { expectedVersion: body.expectedVersion, readState: body.readState, principalId } };
    }
    if (path[5] === "media-access" && method === "POST") return { body: { principalId } };
    return false;
  }

  // ---- Self-service forwarding (CAL-006): rules of the signed-in principal only ----
  if (route === "v1/enterprise/forwarding-rules") {
    if (method === "GET") {
      url.searchParams.set("principalId", principalId);
      return true;
    }
    if (method === "POST" && body) {
      requireSelfDestination(body.destination, principalId);
      return { body: { principalId, condition: body.condition, destination: body.destination } };
    }
    return false;
  }
  if (path.length === 4 && path.slice(0, 3).join("/") === "v1/enterprise/forwarding-rules" && uuid.test(path[3])) {
    if (!["GET", "PATCH", "DELETE"].includes(method)) return false;
    await requireOwnForwardingRule(path[3], principalId);
    if (method === "PATCH" && body) {
      requireSelfDestination(body.destination, principalId);
      return { body: { expectedVersion: body.expectedVersion, destination: body.destination } };
    }
    return true;
  }

  if (isAllowedResourceRoute(route, method)) {
    requireResourceAdministrator(principalId, route);
    return true;
  }
  return false;
}

function requireSelfDestination(destination: unknown, principalId: string) {
  if (!destination || typeof destination !== "object") throw new CallingHttpError(400, "VALIDATION_ERROR", "A destination is required.");
  const { type, id } = destination as { type?: unknown; id?: unknown };
  if (typeof type !== "string" || !SELF_FORWARDING_DESTINATIONS.has(type) || typeof id !== "string" || !uuid.test(id)) {
    throw new CallingHttpError(403, "INSUFFICIENT_SCOPE", "Forward your calls to a person, an extension or voicemail.");
  }
  if (type === "PRINCIPAL" && id === principalId) {
    throw new CallingHttpError(400, "VALIDATION_ERROR", "Calls cannot be forwarded to yourself.");
  }
}

async function authorizeCallRoute(
  identity: CallingIdentity,
  path: string[],
  method: string,
  url: URL,
  body: Body | undefined,
  requireOwnEndpoint: (id: unknown) => Promise<void>,
): Promise<Decision> {
  const principalId = identity.principalId;
  const call: Call = await ownCall(identity, path[2]);
  // Historical participation proves ownership for reads and idempotent replays after
  // termination; `mayControl` decides commands (ADR 0009 party rules, CAL-002).
  const mine = ownParticipant(call, principalId);
  const active = activeParticipant(call, principalId);
  if (!mine) return false;
  const op = path[3];
  if (path.length === 3) return method === "GET";
  if (path.length === 4 && op === "actions") return method === "GET";

  if (path.length === 4 && ["cancel", "end", "hold", "resume", "transfer"].includes(op) && body) {
    if (method !== "POST" || !mayControl(call, principalId, op as ControlOperation)) return false;
    if (op === "transfer") body.transferringParticipantId = mine.participantId;
    return true;
  }
  if (path.length === 4 && ["accept", "decline"].includes(op) && body) {
    if (method !== "POST") return false;
    await requireOwnEndpoint(body.endpointId);
    return call.invitations.some((i) => i.invitationId === body.invitationId && i.endpointId === body.endpointId && i.participantId === mine.participantId);
  }
  if (path.length === 5 && op === "ivr" && path[4] === "input" && body) {
    if (method !== "POST" || !mayControl(call, principalId, "ivrInput")) return false;
    await requireOwnEndpoint(body.endpointId);
    return true;
  }
  if (path.length === 6 && op === "sessions" && uuid.test(path[4]) && ["media-credentials", "remove"].includes(path[5])) {
    return method === "POST" && call.sessions.some((s) => s.sessionId === path[4] && s.participantId === mine.participantId);
  }

  // ---- Recording consent / controls (CAL-005): the participant acts for itself only ----
  if (path.length === 4 && (op === "recording-authorizations" || op === "recordings") && method === "GET") {
    url.search = `?principalId=${encodeURIComponent(principalId)}`;
    return true;
  }
  if (path.length === 4 && op === "recording-consent" && method === "POST" && body) {
    if (!active || typeof body.decision !== "string" || !CONSENT_DECISIONS.has(body.decision)) return false;
    await requireOwnEndpoint(body.endpointId);
    // Consent is taken through the participant's own endpoint with a session in this Call.
    if (!call.sessions.some((s) => s.participantId === active.participantId && s.endpointId === body.endpointId)) return false;
    return { body: { endpointId: body.endpointId, decision: body.decision } };
  }
  if (path.length === 4 && op === "recordings" && method === "POST") {
    return active ? { body: { principalId } } : false;
  }
  if (path.length === 6 && op === "recordings" && uuid.test(path[4]) && path[5] === "stop" && method === "POST") {
    return active ? { body: { principalId } } : false;
  }

  // ---- Abuse report (CAL-005): any (historical) participant reports the Call ----
  if (path.length === 4 && op === "abuse-reports" && method === "POST" && body) {
    if (typeof body.category !== "string" || !ABUSE_CATEGORIES.has(body.category)) {
      throw new CallingHttpError(400, "VALIDATION_ERROR", "Choose a report category.");
    }
    const commentCode = typeof body.commentCode === "string" && /^[A-Z0-9_]{1,64}$/.test(body.commentCode) ? body.commentCode : undefined;
    return { body: { principalId, category: body.category, ...(commentCode ? { commentCode } : {}) } };
  }
  return false;
}

export { handle as GET, handle as POST, handle as PATCH, handle as PUT, handle as DELETE };
