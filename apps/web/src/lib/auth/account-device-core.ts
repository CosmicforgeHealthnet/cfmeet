/**
 * Account device of this browser profile (multi-device policy, ADR 0060): one installation
 * id and one Ed25519 key per browser profile, shared by all its tabs. The private key is a
 * NON-EXTRACTABLE Web Crypto key kept in IndexedDB; it signs proofs of possession for the
 * Account authority (declaration, refresh, device linking). No UI here.
 *
 * Proof statement (docs/architecture/multi-device-linked-devices-handover.md §2):
 *   cfmeet-account-device-proof:v1\n{PURPOSE}\n{keyThumbprint}\n{context}\n{issuedAt}\n{nonce}
 */

export const PROOF_DOMAIN = "cfmeet-account-device-proof:v1";

export type ProofPurpose = "DECLARE" | "REFRESH" | "LINK_REQUEST" | "LINK_APPROVE" | "LINK_COMPLETE" | "REVOKE";

export interface DeviceProof {
  issuedAt: number;
  nonce: string;
  signature: string;
}

/** What this browser remembers about its account device. */
export interface StoredAccountDevice {
  installationId: string;
  privateKey: CryptoKey;
  publicKey: Uint8Array;
  /** The account device id once declared. */
  deviceId?: string;
  /** The sign-in (`sid`) this device was last declared for (refresh proofs use it). */
  sessionId?: string;
}

export interface DeviceView {
  deviceId: string;
  deviceClass: string;
  platform: string;
  state: "ACTIVE" | "REPLACEMENT_PENDING" | "REVOKED";
  trust: "TRUSTED" | "UNTRUSTED";
  keyThumbprint: string;
  primary: boolean;
}

export interface LinkView {
  linkId: string;
  deviceId: string;
  status: "PENDING" | "APPROVED" | "COMPLETED" | "CANCELLED" | "EXPIRED" | "REJECTED";
  expiresAt: string;
  comparisonCode?: string;
  platform?: string;
  label?: string | null;
  keyThumbprint?: string;
}

export interface AccountDeviceApi {
  declare(body: unknown): Promise<{ device: DeviceView }>;
  createLink(body: unknown): Promise<LinkView>;
  getLink(linkId: string): Promise<LinkView>;
  completeLink(linkId: string, body: unknown): Promise<{ device: DeviceView }>;
  pendingLinks(): Promise<{ links: LinkView[] }>;
  approveLink(linkId: string, body: unknown): Promise<LinkView>;
  rejectLink(linkId: string): Promise<LinkView>;
  /** Emails a step-up code for this browser's own link (no trusted device at hand). */
  emailCode(linkId: string): Promise<LinkView>;
}

export interface AccountDeviceDeps {
  subtle: SubtleCrypto;
  randomBytes: (n: number) => Uint8Array;
  /** Per-account storage: one device key per account in this browser profile. */
  load: (account: string) => Promise<StoredAccountDevice | null>;
  save: (account: string, device: StoredAccountDevice) => Promise<void>;
  /** The account whose sign-in this browser last declared (refresh proofs use it). */
  loadCurrent: () => Promise<string | null>;
  saveCurrent: (account: string) => Promise<void>;
  api: AccountDeviceApi;
  now?: () => number;
}

export const b64u = (bytes: Uint8Array): string => {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};

export function proofStatement(purpose: ProofPurpose, thumbprint: string, context: string, issuedAt: number, nonce: string): string {
  return `${PROOF_DOMAIN}\n${purpose}\n${thumbprint}\n${context}\n${issuedAt}\n${nonce}`;
}

function claim(token: string, name: "sid" | "sub"): string | null {
  const part = token.split(".")[1];
  if (!part) return null;
  try {
    const json = JSON.parse(atob(part.replace(/-/g, "+").replace(/_/g, "/")));
    return typeof json[name] === "string" ? json[name] : null;
  } catch {
    return null;
  }
}

/** The `sid` claim of a JWT (no verification: the server decides). */
export const sessionIdOf = (token: string) => claim(token, "sid");
/** The `sub` (account) claim of a JWT. */
export const accountOf = (token: string) => claim(token, "sub");

export function createAccountDevice(deps: AccountDeviceDeps) {
  const now = deps.now ?? (() => Date.now());
  const cache = new Map<string, StoredAccountDevice>();
  /** One declaration per sign-in at a time (sign-in and Messaging may both ask). */
  const declaring = new Map<string, Promise<DeviceView | null>>();

  async function thumbprint(publicKey: Uint8Array): Promise<string> {
    const digest = new Uint8Array(await deps.subtle.digest("SHA-256", publicKey as BufferSource));
    return Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join("");
  }

  /** This browser's device key for `account`, generated once (null without Ed25519). */
  async function ensure(account: string): Promise<StoredAccountDevice | null> {
    const hit = cache.get(account) ?? (await deps.load(account));
    if (hit) {
      cache.set(account, hit);
      return hit;
    }
    let pair: CryptoKeyPair;
    try {
      pair = (await deps.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"])) as CryptoKeyPair;
    } catch {
      return null; // no Ed25519 in this browser: sign-ins stay unclassified
    }
    const publicKey = new Uint8Array(await deps.subtle.exportKey("raw", pair.publicKey));
    const device: StoredAccountDevice = {
      installationId: `web-${b64u(deps.randomBytes(24))}`,
      privateKey: pair.privateKey,
      publicKey,
    };
    await deps.save(account, device);
    cache.set(account, device);
    return device;
  }

  /** The device of the account this browser last declared. */
  async function current(): Promise<StoredAccountDevice | null> {
    const account = await deps.loadCurrent();
    if (!account) return null;
    return cache.get(account) ?? (await deps.load(account));
  }

  async function proof(device: StoredAccountDevice, purpose: ProofPurpose, context: string): Promise<DeviceProof> {
    const nonce = b64u(deps.randomBytes(18));
    const issuedAt = Math.floor(now() / 1000);
    const statement = proofStatement(purpose, await thumbprint(device.publicKey), context, issuedAt, nonce);
    const signature = new Uint8Array(
      await deps.subtle.sign({ name: "Ed25519" }, device.privateKey, new TextEncoder().encode(statement) as BufferSource),
    );
    return { issuedAt, nonce, signature: b64u(signature) };
  }

  async function declared(): Promise<StoredAccountDevice> {
    const device = await current();
    if (!device?.deviceId) throw new Error("DEVICE_NOT_DECLARED");
    return device;
  }

  return {
    thumbprint,

    /** Proof for the next refresh of this browser's sign-in, when it is device-bound. */
    async refreshProof(): Promise<DeviceProof | null> {
      const device = await current();
      if (!device?.sessionId) return null;
      return proof(device, "REFRESH", device.sessionId);
    },

    /**
     * Binds the sign-in of `accessToken` to this browser's device of that account
     * (idempotent per sign-in). Must run within a few minutes of signing in.
     */
    declare(accessToken: string, label = "CF Meet web browser"): Promise<DeviceView | null> {
      const sid = sessionIdOf(accessToken);
      if (!sid) return Promise.resolve(null);
      const inflight = declaring.get(sid);
      if (inflight) return inflight;
      const run = this.declareOnce(accessToken, label).finally(() => declaring.delete(sid));
      declaring.set(sid, run);
      return run;
    },

    async declareOnce(accessToken: string, label: string): Promise<DeviceView | null> {
      const account = accountOf(accessToken);
      const sid = sessionIdOf(accessToken);
      if (!account || !sid) return null;
      const device = await ensure(account);
      if (!device) return null;
      const { device: view } = await deps.api.declare({
        installationId: device.installationId,
        deviceClass: "WEB",
        platform: "WEB",
        label: label.slice(0, 80),
        publicKey: b64u(device.publicKey),
        proof: await proof(device, "DECLARE", sid),
      });
      const updated = { ...device, deviceId: view.deviceId, sessionId: sid };
      await deps.save(account, updated);
      await deps.saveCurrent(account);
      cache.set(account, updated);
      return view;
    },

    /** New, untrusted browser: ask a trusted device to approve it (shows the returned code). */
    async requestLink(): Promise<LinkView> {
      const device = await declared();
      return deps.api.createLink({ proof: await proof(device, "LINK_REQUEST", device.deviceId!) });
    },

    linkStatus: (linkId: string) => deps.api.getLink(linkId),

    async completeLink(linkId: string, emailCode?: string): Promise<DeviceView> {
      const device = await declared();
      const body: Record<string, unknown> = { proof: await proof(device, "LINK_COMPLETE", linkId) };
      if (emailCode) body.emailCode = emailCode;
      return (await deps.api.completeLink(linkId, body)).device;
    },

    pendingLinks: async () => (await deps.api.pendingLinks()).links,

    /** Trusted browser: approve a new device after the user typed the code it shows. */
    async approveLink(link: LinkView, comparisonCode: string): Promise<LinkView> {
      const device = await declared();
      if (!link.keyThumbprint) throw new Error("LINK_WITHOUT_KEY");
      return deps.api.approveLink(link.linkId, {
        comparisonCode: comparisonCode.trim(),
        proof: await proof(device, "LINK_APPROVE", `${link.linkId}|${link.keyThumbprint}`),
      });
    },

    rejectLink: (linkId: string) => deps.api.rejectLink(linkId),

    /** Web-only recovery: email a step-up code for this browser's pending link. */
    sendEmailCode: (linkId: string) => deps.api.emailCode(linkId),
  };
}

export type AccountDevice = ReturnType<typeof createAccountDevice>;
