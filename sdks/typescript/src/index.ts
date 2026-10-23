// CF Meet platform SDK (TypeScript) — Part VI §4 scaffolding.
//
// Everything under src/generated/ is produced by sdks/generate.py from the published
// contracts and must not be edited. The typed API client, credential handling, retries,
// pagination and WebSocket support are Section 52 and are deliberately absent.

export { CONTRACTS, SDK_STATUS } from './generated/contracts.js';
export type { ContractRef } from './generated/contracts.js';

/** The public API Gateway error envelope (contract cfmeet.api-gateway.common `Error`). */
export interface ErrorEnvelope {
  readonly error: {
    readonly code: string;
    readonly message: string;
    readonly requestId: string | null;
  };
}
