import { callingFailure, callingFetch, callingIdentity, CallingHttpError, ownCall, ownEndpoints } from "@/lib/calling/server";

export const runtime = "nodejs";
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
type Context = { params: Promise<{ path: string[] }> };

async function handle(request: Request, context: Context) {
  try {
    const identity = await callingIdentity(request);
    const { path } = await context.params;
    if (request.method === "GET" && path.join("/") === "identity") {
      return Response.json(identity, { headers: { "Cache-Control": "no-store" } });
    }
    const method = request.method;
    const url = new URL(request.url);
    let body: Record<string, unknown> | undefined;
    if (method === "POST") {
      const text = await request.text();
      if (text.length > 16_384) throw new CallingHttpError(413, "VALIDATION_ERROR", "Request too large.");
      try { body = JSON.parse(text || "{}"); } catch { throw new CallingHttpError(400, "VALIDATION_ERROR", "Invalid JSON."); }
      if (!body || Array.isArray(body) || typeof body !== "object") throw new CallingHttpError(400, "VALIDATION_ERROR", "Expected a JSON object.");
    }
    const route = path.join("/");
    let allowed = false;
    const requireOwnEndpoint = async (id: unknown) => {
      const endpoints = await ownEndpoints(identity);
      if (!endpoints.data.some((e) => e.endpointId === id && e.status === "ACTIVE")) {
        throw new CallingHttpError(404, "NOT_FOUND", "Device not found.");
      }
    };
    if (route === "v1/calling/endpoints") {
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

export { handle as GET, handle as POST, handle as DELETE };
