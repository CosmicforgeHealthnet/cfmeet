import { NextResponse, type NextRequest } from "next/server";

import { buildCsp } from "@/lib/security/csp";

/**
 * Issues a fresh CSP nonce per document request. Next.js reads the nonce from the
 * request's Content-Security-Policy header and applies it to its own scripts; the root
 * layout passes it to the components that render inline scripts (next-themes).
 */
export function proxy(request: NextRequest) {
  const bytes = new Uint8Array(18);
  crypto.getRandomValues(bytes);
  const nonce = btoa(String.fromCharCode(...bytes));
  const csp = buildCsp({
    nonce,
    dev: process.env.NODE_ENV === "development",
    secure:
      request.nextUrl.protocol === "https:" ||
      request.headers.get("x-forwarded-proto") === "https",
    apiUrl: process.env.NEXT_PUBLIC_API_URL,
    extraConnectSrc: process.env.CSP_EXTRA_CONNECT_SRC,
    // A WebAssembly exception is permitted only for the audited messaging UI
    // and only when explicitly enabled. All other pages retain no-eval CSP.
    allowMessagingWasm:
      process.env.CFMEET_WEB_MESSAGING_WASM === "enabled" &&
      (request.nextUrl.pathname === "/dashboard/messages" ||
        request.nextUrl.pathname.startsWith("/dashboard/messages/")),
  });
  const headers = new Headers(request.headers);
  headers.set("x-nonce", nonce);
  headers.set("Content-Security-Policy", csp);
  const response = NextResponse.next({ request: { headers } });
  response.headers.set("Content-Security-Policy", csp);
  return response;
}

export const config = {
  matcher: [
    {
      source: "/((?!_next/static|_next/image|favicon.ico|icon.png).*)",
      missing: [
        { type: "header", key: "next-router-prefetch" },
        { type: "header", key: "purpose", value: "prefetch" },
      ],
    },
  ],
};
