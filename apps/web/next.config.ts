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
