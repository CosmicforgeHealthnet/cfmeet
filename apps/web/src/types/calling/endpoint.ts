/**
 * Application endpoint registry and signaling credentials.
 * Mirrors packages/contracts/calling/control/v1.schema.json.
 */

import type {
  CallEndpointKind,
  CallEndpointPresence,
  CallEndpointStatus,
  Uuid,
} from "./common";

export interface EndpointCapabilities {
  audio: boolean;
  video: boolean;
}

export interface RegisterEndpointRequest {
  principalId: Uuid;
  endpointKey: string;
  endpointKind: CallEndpointKind;
  capabilities: EndpointCapabilities;
  pushTargetRef?: Uuid | null;
}

export interface Endpoint {
  endpointId: Uuid;
  principalId: Uuid;
  endpointKey: string;
  endpointKind: CallEndpointKind;
  capabilities: EndpointCapabilities;
  status: CallEndpointStatus;
  presence: CallEndpointPresence;
  lastSeenAt: string | null;
  pushTargetRef: Uuid | null;
  createdAt: string;
  revokedAt: string | null;
}

export interface EndpointList {
  data: Endpoint[];
}

export interface SignalingCredentialRequest {
  principalId: Uuid;
  endpointId: Uuid;
}

export interface SignalingCredential {
  endpointId: Uuid;
  principalId: Uuid;
  token: string;
  expiresAt: string;
  signalingUrl: "/v1/calling/signaling";
}
