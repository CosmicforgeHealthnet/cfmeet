import type { NextConfig } from "next";

import {
  MESSAGING_CRYPTO_PATH,
  MESSAGING_CRYPTO_WORKER_CSP,
  SECURITY_HEADERS,
} from "./src/lib/security/csp";

const nextConfig: NextConfig = {
  poweredByHeader: false,
  async headers() {
    return [
      { source: "/:path*", headers: SECURITY_HEADERS },
      {
        // Content-addressed, SHA-384-pinned crypto worker artifact: immutable, same-origin
        // only, and the only place WebAssembly may be compiled.
        source: `${MESSAGING_CRYPTO_PATH}/:path*`,
        headers: [
          { key: "Content-Security-Policy", value: MESSAGING_CRYPTO_WORKER_CSP },
          { key: "Cache-Control", value: "public, max-age=31536000, immutable" },
          { key: "Cross-Origin-Resource-Policy", value: "same-origin" },
        ],
      },
    ];
  },
  // Local development only: /dev-gateway/* is forwarded to the real API Gateway, so the
  // developer console can be tested from localhost without the gateway allowing localhost
  // (CORS). Point NEXT_PUBLIC_DEVELOPER_API_BASE_URL at http://localhost:3000/dev-gateway.
  // Never active in production builds.
  async rewrites() {
    if (process.env.NODE_ENV !== "development") return [];
    const target = process.env.DEV_GATEWAY_TARGET || "https://api.cfmeet.com";
    return [{ source: "/dev-gateway/:path*", destination: `${target}/:path*` }];
  },
  images: {
    remotePatterns: [
      {
        protocol: "https",
        hostname: "i.pravatar.cc",
        port: "",
        pathname: "/**",
      },
    ],
  },
};

export default nextConfig;
