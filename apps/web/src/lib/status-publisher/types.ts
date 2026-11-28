/**
 * Non-blocking status publishing (WhatsApp-style): the public types.
 *
 * Type-only module: node tests import the engine (queue.ts) directly with
 * `--experimental-strip-types`, so nothing here may emit runtime code.
 */

export type StatusMediaKind = "IMAGE" | "VIDEO" | "AUDIO";

/** Who may see a status (ADR 0069); omitted: the account's saved default, applied by the server. */
export interface StatusAudience {
  mode: "CONTACTS" | "CONTACTS_EXCEPT" | "ONLY" | "CLOSE_FRIENDS" | "CUSTOM";
  principalIds?: string[];
  listId?: string;
}

/** Per-status choices sent with every create of a job (every split segment gets the same ones). */
export interface StatusCreateExtras {
  audience?: StatusAudience;
  allowSharing?: boolean;
  /** Principals mentioned in the caption / text (the server keeps only those inside the audience). */
  mentions?: string[];
}

/** The body of one status creation (the existing CreateStatus shape, media id filled by the queue). */
export type StatusCreateBody =
  | { kind: "TEXT"; text: string; background?: string; font?: number }
  | { kind: "IMAGE" | "AUDIO"; mediaId: string; caption?: string }
  | { kind: "VIDEO"; mediaId: string; startMs: number; endMs: number; caption?: string };

/** What the queue tells the transport when it creates a status (body + ordering metadata). */
export type StatusCreateRequest = StatusCreateBody & StatusCreateExtras & {
  /** Every status of one `enqueueStatus` call shares a groupId… */
  groupId?: string;
  /** …and is published in this order (0, 1, 2…); N only after N-1 is published. */
  sequence?: number;
};

// ---------------------------------------------------------------------------------------------
// Transport: everything the queue needs from the server. Swap the implementation (one file:
// social-transport.ts) to move to the Messaging-backed upload API.
// ---------------------------------------------------------------------------------------------

export type TransportErrorKind =
  /** No connection / request never answered (timeouts included). Retried. */
  | "network"
  /** 5xx. Retried. */
  | "server"
  /** 429. Retried (later). */
  | "rate"
  /** The upload session is gone or unusable (404 / 409 / 410): restart that one upload. */
  | "gone"
  /** The request was refused for good (validation, size, permission). Not retried automatically. */
  | "rejected"
  /** The session ended (401 / 403). Not retried automatically. */
  | "auth"
  /** The request was aborted by the queue (cancel). */
  | "aborted";

/** The error shape transports throw. */
export interface TransportError {
  readonly transportError: true;
  readonly kind: TransportErrorKind;
  readonly status?: number;
  readonly message: string;
}

export interface StartUploadRequest {
  kind: StatusMediaKind;
  contentType: string;
  sizeBytes: number;
  durationMs: number | null;
}

export interface StartedUpload {
  uploadId: string;
  /** Bytes per part (the last part may be shorter). */
  partSize: number;
  partCount: number;
}

export interface TransportCapabilities {
  /** `storedParts` answers which parts the server already has (resume after reload). */
  listParts: boolean;
  /** `createStatus` honours the idempotency key (a retried create is answered as a replay). */
  idempotentCreate: boolean;
  /** `cancelUpload` really discards the server-side upload. */
  cancelUpload: boolean;
}

export interface StatusTransport {
  readonly capabilities: TransportCapabilities;
  startUpload(req: StartUploadRequest, signal: AbortSignal): Promise<StartedUpload>;
  /** The part indexes the server already stores, or null when the transport can't tell. */
  storedParts(uploadId: string, signal: AbortSignal): Promise<number[] | null>;
  uploadPart(uploadId: string, index: number, body: Blob, signal: AbortSignal, onProgress?: (sentBytes: number) => void): Promise<void>;
  completeUpload(uploadId: string, signal: AbortSignal): Promise<{ mediaId: string }>;
  /** Best effort; never throws. */
  cancelUpload(uploadId: string): Promise<void>;
  createStatus(req: StatusCreateRequest, idempotencyKey: string, signal: AbortSignal): Promise<{ postId: string }>;
  /**
   * Only needed when `capabilities.idempotentCreate` is false: after an ambiguous create
   * failure, the caller's existing statuses matching `req` created at or after `since` (ISO),
   * newest last. The queue skips ids it already attributed to other items.
   */
  findCreatedStatus?(req: StatusCreateRequest, since: string, signal: AbortSignal): Promise<{ postId: string }[]>;
}

// ---------------------------------------------------------------------------------------------
// Job input (what editors / the composer hand to `enqueueStatus`).
// ---------------------------------------------------------------------------------------------

/** A processed video piece (≤ 60 s). `durationMs` is probed when missing. */
export type VideoSegmentInput = Blob | { blob: Blob; durationMs: number };

export interface ProcessorContext {
  signal: AbortSignal;
  /** 0–1 */
  onProgress: (fraction: number) => void;
}

export type ProcessorResult =
  /** Ordered ≤ 60 s pieces, each uploaded and posted on its own. */
  | { segments: { blob: Blob; durationMs: number }[]; thumbnail?: Blob }
  /** Processing isn't possible here after all: upload `source` once and post these ranges of it. */
  | { source: Blob; durationMs: number; ranges: { startMs: number; endMs: number }[]; thumbnail?: Blob };

/** A named, registered processor (`registerStatusProcessor`); persisted as name + input + params, so it re-runs after a reload. */
export type StatusProcessor = (input: Blob, params: unknown, ctx: ProcessorContext) => Promise<ProcessorResult>;

export type StatusItemInput = { mentions?: string[] } & (
  | { kind: "TEXT"; text: string; background?: string; font?: number }
  | { kind: "IMAGE"; blob: Blob; caption?: string; preview?: Blob }
  | { kind: "AUDIO"; blob: Blob; durationMs: number; caption?: string; background?: string }
  /** Already processed ≤ 60 s pieces: each is uploaded and posted on its own, in order. */
  | { kind: "VIDEO"; segments: VideoSegmentInput[]; caption?: string; thumbnail?: Blob }
  /** One source uploaded once and posted as consecutive ranges (today's server model). */
  | { kind: "VIDEO"; source: Blob; durationMs: number; ranges: { startMs: number; endMs: number }[]; caption?: string; thumbnail?: Blob }
  /**
   * Deferred processing after the editor closed. `processor` + `input` + `params` (JSON) are
   * persisted and re-run after a reload; the outputs are persisted as soon as produced.
   */
  | { kind: "VIDEO"; process: { processor: string; input: Blob; params?: unknown }; caption?: string; thumbnail?: Blob }
  /**
   * Deferred processing with a closure (not persistable): outputs are persisted as soon as
   * produced; a reload before that marks the job failed ("edit it again"), cancel only.
   */
  | { kind: "VIDEO"; prepare: (ctx: ProcessorContext) => Promise<ProcessorResult>; caption?: string; thumbnail?: Blob });

export interface StatusJobInput {
  items: StatusItemInput[];
  /** The audience chosen in the editor (omitted: the saved default). Applies to every item and segment. */
  audience?: StatusAudience;
  /** Allow sharing (omitted: the saved default). */
  allowSharing?: boolean;
  /** What the audience button read ("My contacts", "8 excluded", "Close friends", a list's name). */
  audienceLabel?: string;
}

// ---------------------------------------------------------------------------------------------
// Persistence (IndexedDB in the browser, memory in tests).
// ---------------------------------------------------------------------------------------------

export interface QueueStorage {
  loadJobs(): Promise<StoredJob[]>;
  /** Job records hold blob keys, never Blobs (cheap to rewrite on every progress step). */
  putJob(job: StoredJob): Promise<void>;
  deleteJob(id: string): Promise<void>;
  putBlob(key: string, blob: Blob): Promise<void>;
  getBlob(key: string): Promise<Blob | null>;
  deleteBlob(key: string): Promise<void>;
  /** Removes every blob whose key starts with `${jobId}/`. */
  deleteBlobs(jobId: string): Promise<void>;
  clear(): Promise<void>;
}

export interface StoredUpload {
  key: string;
  /** Index of the input item it belongs to. */
  item: number;
  blobKey: string;
  kind: StatusMediaKind;
  contentType: string;
  sizeBytes: number;
  durationMs: number | null;
  uploadId: string | null;
  partSize: number | null;
  partCount: number | null;
  /** Part indexes the server acknowledged. */
  partsDone: number[];
  mediaId: string | null;
}

export interface StoredPost {
  sequence: number;
  /** Index of the input item it belongs to. */
  item: number;
  /** Stands for a deferred video's pieces until processing produced them (blocks later posts). */
  placeholder?: boolean;
  /** Generated once when the job is enqueued; reused by every retry. */
  idempotencyKey: string;
  uploadKey: string | null;
  /** The create body without `mediaId` (filled from the upload). */
  body: Record<string, unknown> & { kind: string };
  postId: string | null;
  /** A create was sent and its outcome is unknown (timeout / lost response). */
  uncertain: boolean;
}

export interface StoredProcess {
  itemIndex: number;
  /** "named" re-runs after reload; "closure" cannot. */
  mode: "named" | "closure";
  processor: string | null;
  inputBlobKey: string | null;
  params: unknown;
  caption: string | null;
  /** Mentions of the caption (sent with the first piece, like the caption). */
  mentions?: string[] | null;
}

export type StoredPreview =
  | { kind: "TEXT"; text: string; background: string | null; font: number }
  | { kind: "IMAGE" | "VIDEO" | "AUDIO"; blobKey: string | null; background?: string | null; videoBlobKey?: string | null };

export interface StoredJob {
  id: string;
  groupId: string;
  ownerId: string | null;
  createdAt: string;
  /** Ordering across jobs (monotonic per browser). */
  order: number;
  posts: StoredPost[];
  uploads: StoredUpload[];
  process: StoredProcess | null;
  /** The job started with deferred processing (progress weighting). */
  hasProcess: boolean;
  preview: StoredPreview;
  attempts: number;
  nextAttemptAt: number;
  failed: boolean;
  error: string | null;
  /** The error is permanent: retrying the same request cannot help (cancel only). */
  fatal: boolean;
  /** Sent with every create of the job (absent on jobs queued before audiences existed). */
  audience?: StatusAudience | null;
  allowSharing?: boolean | null;
  audienceLabel?: string | null;
}

// ---------------------------------------------------------------------------------------------
// What the UI reads.
// ---------------------------------------------------------------------------------------------

export type JobState = "queued" | "processing" | "uploading" | "publishing" | "waiting" | "failed" | "done";

export type JobPreview =
  | { kind: "TEXT"; text: string; background: string | null; font: number }
  | { kind: "IMAGE" | "VIDEO" | "AUDIO"; blob: Blob | null; video?: Blob | null; background?: string | null };

export interface JobView {
  id: string;
  groupId: string;
  createdAt: string;
  state: JobState;
  /** 0–1 across processing + upload + publish. */
  progress: number;
  error: string | null;
  /** Retry cannot help (e.g. processing interrupted by a reload). */
  fatal: boolean;
  preview: JobPreview;
  postCount: number;
  publishedCount: number;
  /** The audience button's label when it was queued (null: the saved default). */
  audienceLabel: string | null;
}

export interface QueueSnapshot {
  jobs: JobView[];
  online: boolean;
  /** Pending jobs exist only on this device until the server confirms each status. */
  persistent: boolean;
}

export type QueueEvent =
  | { type: "published"; job: JobView }
  | { type: "failed"; job: JobView };
