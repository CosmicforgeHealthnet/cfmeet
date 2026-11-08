/**
 * Browser session cookie of the web app (ACC-019).
 *
 * POST   { refresh_token, remember }  stores the refresh token from a sign-in the browser
 *                                     just made with the account authority (password login
 *                                     or Google exchange) in an httpOnly cookie.
 * DELETE                              removes the session cookies (sign-out; the API session
 *                                     itself is ended by POST /api/v1/auth/logout).
 *
 * Same-origin only (custom header + Origin / Sec-Fetch-Site), never cached.
 */
import {
  clearedSessionCookies,
  isSameOriginSessionRequest,
  isSecureRequest,
  looksLikeJwt,
  sessionCookies,
} from "@/lib/auth/session-cookies";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "Cache-Control": "no-store" };

function refuse(status: number, code: string, message: string) {
  return Response.json({ success: false, error: { code, message } }, { status, headers: NO_STORE });
}

export async function POST(request: Request) {
  if (!isSameOriginSessionRequest(request.headers, request.url)) {
    return refuse(403, "PERMISSION_DENIED", "Session requests must come from this site.");
  }
  const body = (await request.json().catch(() => null)) as { refresh_token?: unknown; remember?: unknown } | null;
  if (!body || !looksLikeJwt(body.refresh_token) || (body.remember !== undefined && typeof body.remember !== "boolean")) {
    return refuse(400, "VALIDATION_ERROR", "A refresh token is required.");
  }
  const response = Response.json({ success: true }, { headers: NO_STORE });
  for (const cookie of sessionCookies({
    refreshToken: body.refresh_token,
    persistent: body.remember === true,
    secure: isSecureRequest(request.headers, request.url),
    nowMs: Date.now(),
  })) {
    response.headers.append("Set-Cookie", cookie);
  }
  return response;
}

export async function DELETE(request: Request) {
  if (!isSameOriginSessionRequest(request.headers, request.url)) {
    return refuse(403, "PERMISSION_DENIED", "Session requests must come from this site.");
  }
  const response = new Response(null, { status: 204, headers: NO_STORE });
  for (const cookie of clearedSessionCookies(isSecureRequest(request.headers, request.url))) {
    response.headers.append("Set-Cookie", cookie);
  }
  return response;
}
