/**
 * POST /api/session/refresh — rotates the httpOnly refresh cookie with the account
 * authority (`POST /api/v1/auth/refresh`) and returns only the new access token, which the
 * browser keeps in memory (ACC-019). Refresh tokens are single-use (ADR 0057); the browser
 * serializes calls to this route across tabs (Web Locks).
 */
import {
  HINT_COOKIE,
  REFRESH_COOKIE,
  accountRefreshUrl,
  clearedSessionCookies,
  isSameOriginSessionRequest,
  isSecureRequest,
  looksLikeJwt,
  readCookie,
  refreshOutcome,
  sessionCookies,
} from "@/lib/auth/session-cookies";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "Cache-Control": "no-store" };

function answer(status: number, body: unknown, cookies: string[] = []) {
  const response = Response.json(body, { status, headers: NO_STORE });
  for (const cookie of cookies) response.headers.append("Set-Cookie", cookie);
  return response;
}

const failure = (code: string, message: string) => ({ success: false, error: { code, message } });

export async function POST(request: Request) {
  if (!isSameOriginSessionRequest(request.headers, request.url)) {
    return answer(403, failure("PERMISSION_DENIED", "Session requests must come from this site."));
  }
  const secure = isSecureRequest(request.headers, request.url);
  const cookieHeader = request.headers.get("cookie");
  const refreshToken = readCookie(cookieHeader, REFRESH_COOKIE);
  if (!looksLikeJwt(refreshToken)) {
    return answer(401, failure("SESSION_ENDED", "Sign in again."), clearedSessionCookies(secure));
  }
  const persistent = readCookie(cookieHeader, HINT_COOKIE) === "persistent";

  let upstream: Response;
  try {
    // ACCOUNT_API_INTERNAL_URL: the API's address as seen from the web server (container
    // networks); defaults to the public NEXT_PUBLIC_API_URL.
    upstream = await fetch(accountRefreshUrl(process.env.ACCOUNT_API_INTERNAL_URL || process.env.NEXT_PUBLIC_API_URL), {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ refresh_token: refreshToken }),
      cache: "no-store",
      redirect: "error",
      signal: AbortSignal.timeout(10_000),
    });
  } catch {
    // The cookie is kept: the session may still be valid once the API answers again.
    return answer(503, failure("SERVICE_UNAVAILABLE", "The sign-in service is temporarily unavailable."));
  }

  switch (refreshOutcome(upstream.status)) {
    case "ended":
      return answer(401, failure("SESSION_ENDED", "Your session has ended. Sign in again."), clearedSessionCookies(secure));
    case "rate_limited":
      return answer(429, failure("RATE_LIMITED", "Too many requests. Try again shortly."));
    case "unavailable":
      return answer(503, failure("SERVICE_UNAVAILABLE", "The sign-in service is temporarily unavailable."));
    case "rotated":
      break;
  }

  const data = (await upstream.json().catch(() => null)) as { access_token?: unknown; refresh_token?: unknown } | null;
  if (!data || !looksLikeJwt(data.access_token) || !looksLikeJwt(data.refresh_token)) {
    return answer(502, failure("UPSTREAM_ERROR", "The sign-in service returned an invalid answer."));
  }
  return answer(
    200,
    { access_token: data.access_token },
    sessionCookies({ refreshToken: data.refresh_token, persistent, secure, nowMs: Date.now() }),
  );
}
