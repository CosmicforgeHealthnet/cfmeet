/** Meeting Service v2 Unified Messaging bridge. No platform/service secrets. */
export interface MessagingIdentity {
  principalId: string;
  tenantId: string;
  applicationId: string;
  environmentId: string;
}

export interface Colleague {
  userId: string;
  displayName: string;
  principalId: string | null;
}

export type DeviceRevocationReason = "USER_REQUESTED" | "LOST_OR_STOLEN" | "COMPROMISED" | "REPLACED";
export interface MessagingDevice {
  deviceId: string;
  principalId: string;
  displayLabel: string | null;
  status: "ACTIVE" | "REVOKED";
  signaturePublicKey: string;
  registeredAt: string;
  revokedAt: string | null;
  revocationReason: string | null;
  protocol: string;
  ciphersuite: string;
  credentialIdentity: string;
  keyFingerprint: string;
  transparency: { registrationLeafIndex: number; revocationLeafIndex: number | null };
  version: number;
}

export interface RegistrationChallenge {
  challengeId: string;
  deviceId: string;
  principalId: string;
  nonce: string;
  registrationBinding: string;
  credentialIdentity: string;
  protocol: string;
  ciphersuite: string;
  expiresAt: string;
}

export interface RegisterMessagingDeviceRequest {
  challengeId: string;
  keyPackage: string;
  displayLabel: string;
}

export interface MessagingClientToken {
  token: string;
  tokenType: "CF-Client-Token";
  sessionId: string;
  deviceId: string;
  expiresAt: string;
  scopes: string[];
}

export interface PublishedKeyPackages {
  deviceId: string;
  keyPackages: { keyPackageRef: string; notAfter: string; outcome: "PUBLISHED" | "ALREADY_PUBLISHED" }[];
}

export type ChatMigrationState = "LEGACY_PRIMARY" | "SHADOW_VALIDATION" | "MESSAGING_PRIMARY" | "LEGACY_RETIRED";
export interface ChatParityRun {
  id: string;
  status: string;
  startedAt: string;
  finishedAt: string | null;
  conferencesChecked: number;
  messagesChecked: number;
  mismatches: number;
}

export interface ChatMigrationTransitionRequest {
  targetState: ChatMigrationState;
  expectedVersion: number;
  reason: string;
  approvedBy?: string;
}

export interface ChatMigrationStatus {
  state: ChatMigrationState;
  version: number;
  changedAt: string;
  changedBy: string;
  pendingMirror: number;
  pendingRollbackCopies: number;
  pendingSessionRevocations: number;
  linkedConferences: number;
  latestParityRun: ChatParityRun | null;
  transitions: {
    fromState: ChatMigrationState;
    toState: ChatMigrationState;
    version: number;
    actor: string;
    approvedBy: string | null;
    reason: string;
    createdAt: string;
  }[];
  scope: { migrated: string[]; legacy: string[]; securityProfile: string; historicalProvenance: string };
}
