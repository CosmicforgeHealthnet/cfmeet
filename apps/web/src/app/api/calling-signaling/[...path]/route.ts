import { callingFailure, CallingHttpError } from "@/lib/calling/server";

export const runtime = "nodejs";
type Context = { params: Promise<{ path: string[] }> };
const uuid = "[0-9a-fA-F-]{36}";

/** Same-origin HTTP transport; Calling remains the signaling-token authority. */
async function handle(request: Request, context: Context) {
  try {
    const { path } = await context.params;
    const route = path.join("/");
    const read = request.method === "GET" && /^v1\/calling\/(invitations|history)$/.test(route);
    const command = request.method === "POST" && new RegExp(`^v1/calling/(invitations/${uuid}/(accept|decline)|sessions/${uuid}/media-credentials)$`).test(route);
    if (!read && !command) throw new CallingHttpError(404, "NOT_FOUND", "Signaling operation not found.");
    const authorization = request.headers.get("Authorization");
    if (!authorization || !/^Bearer cfsig_[A-Za-z0-9._-]+$/.test(authorization) || authorization.length > 2060) {
      throw new CallingHttpError(401, "AUTHENTICATION_FAILED", "A calling endpoint credential is required.");
    }
    const headers = new Headers({ Authorization: authorization, "Content-Type": "application/json" });
    const key = request.headers.get("Idempotency-Key");
    if (key) headers.set("Idempotency-Key", key);
    const body = command ? await request.text() : undefined;
    if (body && body.length > 4096) throw new CallingHttpError(413, "VALIDATION_ERROR", "Request too large.");
    const base = process.env.CALLING_API_URL || process.env.NEXT_PUBLIC_CALLING_API_URL || "http://localhost:8090";
    const upstream = await fetch(`${base.replace(/\/$/, "")}/${route}${new URL(request.url).search}`, {
      method: request.method, headers, body, cache: "no-store", redirect: "error", signal: AbortSignal.timeout(20_000),
    });
    const responseHeaders = new Headers({ "Content-Type": "application/json", "Cache-Control": "no-store" });
    for (const name of ["Idempotent-Replayed", "Retry-After", "X-Request-Id"]) {
      const value = upstream.headers.get(name);
      if (value) responseHeaders.set(name, value);
    }
    return new Response(upstream.body, { status: upstream.status, headers: responseHeaders });
  } catch (error) { return callingFailure(error); }
}

export { handle as GET, handle as POST };
