/**
 * The page's handle on the Messaging device worker (public/messaging-crypto/). The page
 * never holds key material: it sends MLS messages and ciphertext in, and receives public
 * keys, MLS messages, ciphertext and — for display — decrypted text and files.
 *
 * The worker URL is the content-addressed artifact the build pinned
 * (`crypto-artifact.ts`); under Trusted Types it is the only script URL the `cfmeet`
 * policy accepts.
 */
import { MESSAGING_CRYPTO_WORKER_URL } from "./crypto-artifact";

export type StoredRecord = {
  sequence: number;
  messageId?: string;
  senderPrincipalId?: string;
  senderDeviceId?: string;
  createdAt?: string;
  kind?: string;
  lifecycle?: string;
  replyToMessageId?: string | null;
  targetMessageId?: string | null;
  status: "DECRYPTED" | "SENT" | "SENT_ELSEWHERE" | "COMMIT" | "BEFORE_JOIN" | "REMOVED" | "FAILED";
  error?: string;
  transferred?: boolean;
  beforeJoinVerified?: boolean;
  payload?:
    | { t: "text"; text: string; preview?: LinkPreview }
    | { t: "file"; caption: string | null; attachmentId: string | null; name: string | null; mediaType: string | null }
    | RichPayload;
  fileName?: string;
  mediaType?: string;
  size?: number;
  attachmentId?: string | null;
};

import type { LinkPreview, RichPayload } from "./rich-content";

export type DeviceStatus =
  | { status: "NONE" | "LOCKED" | "CLOSED" }
  | { status: "READY" | "PENDING"; deviceId: string; signaturePublicKey: Uint8Array };

export class CryptoWorkerError extends Error {
  constructor(public readonly code: string) {
    super(code);
    this.name = "CryptoWorkerError";
  }
}

type TrustedTypesLike = {
  createPolicy(name: string, rules: { createScriptURL(u: string): string }): { createScriptURL(u: string): unknown };
};

let scriptPolicy: { createScriptURL(u: string): unknown } | null = null;
const clients = new Set<CryptoClient>();

/** Ends all in-memory cryptographic access in this tab when its Account session ends. */
export function terminateMessagingWorkers(): void {
  for (const client of clients) {
    void client.call("lock").catch(() => {}).finally(() => client.terminate());
  }
}

/** The worker's URL as a TrustedScriptURL where Trusted Types are enforced. */
function workerUrl(): string | URL {
  const tt = (globalThis as { trustedTypes?: TrustedTypesLike }).trustedTypes;
  if (!tt) return MESSAGING_CRYPTO_WORKER_URL;
  scriptPolicy ??= tt.createPolicy("cfmeet", {
    createScriptURL(u: string) {
      if (u !== MESSAGING_CRYPTO_WORKER_URL) throw new TypeError("refused script URL");
      return u;
    },
  });
  return scriptPolicy.createScriptURL(MESSAGING_CRYPTO_WORKER_URL) as string;
}

/** One serialized device runtime shared by authorized tabs of this browser profile. */
export class CryptoClient {
  private worker: Worker | MessagePort | null = null;
  private shared: SharedWorker | null = null;
  private readonly factory?: () => Worker;
  private readonly pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private seq = 0;
  private failed: string | null = null;

  constructor(factory?: () => Worker) {
    this.factory = factory;
    clients.add(this);
  }

  private connect(op: string, args: Record<string, unknown>): void {
    if (this.worker) return;
    if (!this.factory && typeof SharedWorker !== "undefined" && op === "open" && typeof args.account === "string") {
      this.shared = new SharedWorker(workerUrl(), { type: "module", name: `cfmeet-messaging:${args.account}` });
      this.worker = this.shared.port;
      this.shared.port.start();
    } else {
      this.worker = this.factory ? this.factory() : new Worker(workerUrl(), { type: "module", name: "cfmeet-messaging-device" });
    }
    this.worker.onmessage = ({ data }: MessageEvent<{ id: number; ok: boolean; result?: unknown; error?: string }>) => {
      const p = this.pending.get(data.id);
      if (!p) return;
      this.pending.delete(data.id);
      if (data.ok) p.resolve(data.result);
      else p.reject(new CryptoWorkerError(data.error ?? "CRYPTO_ERROR"));
    };
    // A worker that cannot load (missing artifact, CSP) fails every call, never hangs.
    const failed = () => {
      this.failed = "CRYPTO_WORKER_UNAVAILABLE";
      for (const p of this.pending.values()) p.reject(new CryptoWorkerError(this.failed));
      this.pending.clear();
    };
    if (this.shared) this.shared.onerror = failed;
    else (this.worker as Worker).onerror = failed;
  }

  call<T>(op: string, args: Record<string, unknown> = {}): Promise<T> {
    return this.callWithTransfer(op, args, []);
  }

  /** Moves transient authenticator output into the worker without leaving a page copy. */
  callWithTransfer<T>(op: string, args: Record<string, unknown>, transfer: Transferable[]): Promise<T> {
    if (this.failed) return Promise.reject(new CryptoWorkerError(this.failed));
    try { this.connect(op, args); } catch { return Promise.reject(new CryptoWorkerError("CRYPTO_WORKER_UNAVAILABLE")); }
    const id = ++this.seq;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      try {
        this.worker!.postMessage({ id, op, args }, transfer);
      } catch (error) {
        this.pending.delete(id);
        reject(error instanceof Error ? error : new CryptoWorkerError("CRYPTO_WORKER_UNAVAILABLE"));
      }
    });
  }

  terminate(): void {
    clients.delete(this);
    if (this.shared) (this.worker as MessagePort | null)?.close();
    else (this.worker as Worker | null)?.terminate();
    this.worker = null;
    this.shared = null;
    for (const p of this.pending.values()) p.reject(new CryptoWorkerError("CRYPTO_WORKER_CLOSED"));
    this.pending.clear();
    this.failed = "CRYPTO_WORKER_CLOSED";
  }
}
