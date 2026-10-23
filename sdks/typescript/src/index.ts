// CF Meet platform SDK (TypeScript) — Part VI Section 52.
//
// Everything under src/generated/ is produced by sdks/generate.py from the published
// contracts and must not be edited.

export { CONTRACTS, SDK_STATUS } from './generated/contracts.js';
export type { ContractRef } from './generated/contracts.js';
export { OPERATIONS } from './generated/operations.js';
export type { OperationSpec, ParamSpec, PaginationSpec, RetryClass, AuthMode } from './generated/operations.js';
export type * from './generated/models.js';

export { CfMeetClient, DOMAIN_ACCESSORS, readQuota } from './client.js';
export type {
  CfMeetClientConfig,
  DomainClient,
  OperationMethod,
  OperationTypesOf,
  ParamsOf,
  ResponseOf,
} from './client.js';
export type { ApiResponse, GatewayForwardedHeaders, QueryValue, RequestOptions, RequestParams } from './types.js';
export { buildPath } from './path.js';
export { AUTH_MODES, BROWSER_ESCAPE_HATCH, isBrowserLike } from './auth.js';
export type { AuthType, CfMeetAuth, TokenSource } from './auth.js';
export { DEFAULT_RETRY_POLICY, backoffDelay, parseRetryAfter, retryableStatus, retryableTransport } from './retry.js';
export type { RetryPolicy } from './retry.js';
export {
  CfMeetAbortError,
  CfMeetApiError,
  CfMeetAuthenticationError,
  CfMeetConfigurationError,
  CfMeetConflictError,
  CfMeetError,
  CfMeetNetworkError,
  CfMeetNotFoundError,
  CfMeetPermissionError,
  CfMeetQuotaExceededError,
  CfMeetRateLimitedError,
  CfMeetServerError,
  CfMeetTimeoutError,
  CfMeetUpstreamUnavailableError,
  CfMeetValidationError,
  CfMeetWebhookSignatureError,
  UnknownOutcomeError,
} from './errors.js';
export type { ApiErrorKind, OperationRef, QuotaInfo, UnknownOutcomeReason } from './errors.js';
export { RealtimeClient, RealtimeConnection, defaultTerminalClose } from './realtime.js';
export type {
  CallingSignalingOptions,
  MeetingsRealtimeOptions,
  MessagingRealtimeOptions,
  RealtimeCloseInfo,
  RealtimeEndpoint,
  RealtimeEvents,
  RealtimeOptions,
  RealtimeState,
  ReconnectOptions,
  WebSocketConstructorLike,
  WebSocketLike,
} from './realtime.js';
export { DEFAULT_WEBHOOK_TOLERANCE_SECONDS, WEBHOOK_HEADERS, constructEvent, verifyWebhookSignature } from './webhooks.js';
export type { HeaderSource, VerifiedWebhookEvent, VerifyWebhookInput, WebhookEvent, WebhookVerificationResult } from './webhooks.js';
export { MessagingCryptoError, createMessagingCrypto, loadMessagingCrypto, verifyIntegrity } from './messaging-crypto.js';
export type {
  CommitResult,
  DecryptedFile,
  DescriptorInfo,
  EncryptedFile,
  KeyPackageInfo,
  MessageBinding,
  MessagingCrypto,
  MessagingCryptoModule,
  MessagingCryptoSource,
  MessagingCryptoWebGlue,
  ProcessResult,
  TransferContext,
  WebDevice,
  WebDeviceConstructor,
} from './messaging-crypto.js';
export { SDK_VERSION } from './version.js';

/** The public API Gateway error envelope (contract cfmeet.api-gateway.common `Error`). */
export interface ErrorEnvelope {
  readonly error: {
    readonly code: string;
    readonly message: string;
    readonly requestId: string | null;
  };
}
