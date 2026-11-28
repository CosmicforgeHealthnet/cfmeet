/**
 * The publishing queue's transport over the Messaging-backed Status API (ADR 0069), through
 * `social.service.ts` (Meeting Service relay → Messaging):
 *
 *  - listParts: `storedParts` asks the server which parts it already holds (resume after reload).
 *  - idempotentCreate: the queue's key is sent as `Idempotency-Key`; a retried create (within
 *    24 h) answers the original status.
 *  - cancelUpload: discards the unfinished upload and its stored bytes on the server.
 *  - groupId / sequence are sent, so a group shows in order even if created out of order.
 *
 * Parts go straight to protected object storage with per-part signed uploads; a status can only
 * be created from COMPLETE media of the caller, so nothing pending is ever visible to anyone.
 */
import { socialService, type CreateStatus } from "@/services/v2/social.service";
import { isRouteNotAllowed } from "@/services/v2/social-backend";
import { transportError } from "./queue";
import type { StatusCreateRequest, StatusTransport, TransportError } from "./types";

function statusOf(e: unknown): number | undefined {
  const err = e as { response?: { status?: number }; status?: number };
  return err?.response?.status ?? err?.status;
}

function messageOf(e: unknown): string | undefined {
  const data = (e as { response?: { data?: { error?: { message?: string }; message?: string } } })?.response?.data;
  return data?.error?.message ?? data?.message ?? undefined;
}

/** Service error → the queue's error kinds. `phase` decides what 400/404/409 mean. */
function mapError(e: unknown, phase: "start" | "part" | "complete" | "create" | "read"): TransportError {
  // Kept recognisable so the queue's transport can switch backends (TRANSITIONAL, social-backend.ts).
  if (isRouteNotAllowed(e)) return Object.assign(transportError("rejected", "Not served here", 404), { code: "ENDPOINT_NOT_FOUND" });
  if ((e as { name?: string })?.name === "AbortError" || (e as { name?: string })?.name === "CanceledError") {
    return transportError("aborted", "Cancelled");
  }
  const status = statusOf(e);
  const message = messageOf(e) ?? "Failed to upload";
  if (status === undefined || status === 0 || status === 408) return transportError("network", "No connection", status);
  if (status === 401) return transportError("auth", message, status);
  // An expired signed part URL answers 403 from the store: retry the part (a fresh grant).
  if (status === 403) return phase === "part" ? transportError("network", message, status) : transportError("auth", message, status);
  if (status === 429) return transportError("rate", message, status);
  if (status >= 500) return transportError("server", message, status);
  if ((phase === "part" || phase === "complete") && (status === 404 || status === 409 || status === 410)) return transportError("gone", message, status);
  return transportError("rejected", message, status);
}

function toCreateStatus(req: StatusCreateRequest): CreateStatus {
  const order = req.groupId !== undefined && req.sequence !== undefined ? { groupId: req.groupId, sequence: req.sequence } : {};
  switch (req.kind) {
    case "TEXT": return { kind: "TEXT", text: req.text, background: req.background, font: req.font, ...order };
    case "VIDEO": return { kind: "VIDEO", mediaId: req.mediaId, startMs: req.startMs, endMs: req.endMs, caption: req.caption, ...order };
    default: return { kind: req.kind, mediaId: req.mediaId, caption: req.caption, ...order };
  }
}

export function createMessagingTransport(): StatusTransport {
  return {
    capabilities: { listParts: true, idempotentCreate: true, cancelUpload: true },

    async startUpload(req, signal) {
      if (signal.aborted) throw transportError("aborted", "Cancelled");
      try {
        const m = await socialService.startUpload("STATUS", req.kind, req.contentType, req.sizeBytes, req.durationMs);
        return { uploadId: m.mediaId, partSize: m.chunkBytes, partCount: m.chunkCount };
      } catch (e) { throw mapError(e, "start"); }
    },

    async storedParts(uploadId, signal) {
      if (signal.aborted) throw transportError("aborted", "Cancelled");
      try {
        const m = await socialService.uploadState(uploadId, "STATUS");
        return m.complete ? Array.from({ length: m.chunkCount }, (_, i) => i) : (m.storedParts ?? []);
      } catch (e) { throw mapError(e, "part"); }
    },

    async uploadPart(uploadId, index, body, signal, onProgress) {
      try {
        await socialService.uploadPart(uploadId, index, body, "STATUS", signal);
        onProgress?.(body.size);
      } catch (e) { throw mapError(e, "part"); }
    },

    async completeUpload(uploadId, signal) {
      if (signal.aborted) throw transportError("aborted", "Cancelled");
      try {
        const m = await socialService.completeUpload(uploadId, "STATUS");
        return { mediaId: m.mediaId };
      } catch (e) { throw mapError(e, "complete"); }
    },

    async cancelUpload(uploadId) {
      try { await socialService.cancelUpload(uploadId, "STATUS"); } catch { /* best effort */ }
    },

    async createStatus(req, idempotencyKey, signal) {
      if (signal.aborted) throw transportError("aborted", "Cancelled");
      try {
        const post = await socialService.postStatus(toCreateStatus(req), idempotencyKey);
        return { postId: post.postId };
      } catch (e) { throw mapError(e, "create"); }
    },
  };
}
