/**
 * TRANSITIONAL (one release): the publishing queue's transport over the previous Meeting
 * Service status API (`/api/v2/social/*`), used only while the backend still runs the release
 * before #193. TODO(#193 / ADR 0069 "Rollout"): delete after #193 is deployed.
 *
 * Degraded where that API lacks a capability (as before #193):
 *  - listParts: no part listing; after a reload the queue trusts the parts it recorded, and a
 *    `complete` that reports missing pieces (400) or a vanished upload (404) restarts it.
 *  - idempotentCreate: no idempotency key; after an ambiguous create the queue looks for a
 *    matching status of the caller (`findCreatedStatus`) before creating again.
 *  - cancelUpload: no discard endpoint; an unfinished upload expires server-side.
 *  - groupId / sequence are not sent (that API refuses unknown fields).
 */
import axios from "axios";
import { completeUpload, legacySocial, startUpload, uploadPart } from "@/services/v2/social.legacy";
import { isRouteNotAllowed } from "@/services/v2/social-backend";
import type { CreateStatus } from "@/services/v2/social.service";
import { transportError } from "./queue";
import type { StatusCreateRequest, StatusTransport, TransportError } from "./types";

function messageOf(e: unknown): string | undefined {
  const data = (e as { response?: { data?: { error?: { message?: string }; message?: string } } })?.response?.data;
  return data?.error?.message ?? data?.message;
}

/** Axios error → the queue's error kinds. `phase` decides what 400/404/409 mean. */
function mapError(e: unknown, phase: "start" | "part" | "complete" | "create" | "read"): TransportError {
  // Kept recognisable so the queue's transport can switch backends (social-backend.ts).
  if (isRouteNotAllowed(e)) return Object.assign(transportError("rejected", "Not served here", 404), { code: "ENDPOINT_NOT_FOUND" });
  if (axios.isCancel(e) || (e as { name?: string })?.name === "CanceledError" || (e as { name?: string })?.name === "AbortError") {
    return transportError("aborted", "Cancelled");
  }
  const status = (e as { response?: { status?: number } })?.response?.status;
  const message = messageOf(e) ?? "Failed to upload";
  if (status === undefined || status === 0 || status === 408) return transportError("network", "No connection", status);
  if (status === 401 || status === 403) return transportError("auth", message, status);
  if (status === 429) return transportError("rate", message, status);
  if (status >= 500) return transportError("server", message, status);
  if ((phase === "part" || phase === "complete") && (status === 404 || status === 409 || status === 410)) return transportError("gone", message, status);
  // `complete` with pieces missing is a 400: restart that upload rather than give up.
  if (phase === "complete" && status === 400) return transportError("gone", message, status);
  return transportError("rejected", message, status);
}

function toCreateStatus(req: StatusCreateRequest): CreateStatus {
  switch (req.kind) {
    case "TEXT": return { kind: "TEXT", text: req.text, background: req.background, font: req.font };
    case "VIDEO": return { kind: "VIDEO", mediaId: req.mediaId, startMs: req.startMs, endMs: req.endMs, caption: req.caption };
    default: return { kind: req.kind, mediaId: req.mediaId, caption: req.caption };
  }
}

export function createLegacyTransport(): StatusTransport {
  return {
    capabilities: { listParts: false, idempotentCreate: false, cancelUpload: false },

    async startUpload(req, signal) {
      try {
        if (signal.aborted) throw transportError("aborted", "Cancelled");
        const m = await startUpload("STATUS", req.kind, req.contentType, req.sizeBytes, req.durationMs);
        return { uploadId: m.mediaId, partSize: m.chunkBytes, partCount: m.chunkCount };
      } catch (e) { throw mapError(e, "start"); }
    },

    async storedParts() {
      return null; // DEGRADED: no part listing today.
    },

    async uploadPart(uploadId, index, body, signal, onProgress) {
      try {
        await uploadPart(uploadId, index, body, signal);
        onProgress?.(body.size);
      } catch (e) { throw mapError(e, "part"); }
    },

    async completeUpload(uploadId, signal) {
      try {
        if (signal.aborted) throw transportError("aborted", "Cancelled");
        const m = await completeUpload(uploadId, "STATUS");
        return { mediaId: m.mediaId };
      } catch (e) { throw mapError(e, "complete"); }
    },

    async cancelUpload() {
      // DEGRADED: no discard endpoint today; the unfinished upload expires server-side.
    },

    async createStatus(req, _idempotencyKey, signal) {
      // DEGRADED: the key isn't sent (no server support yet); see findCreatedStatus.
      void _idempotencyKey;
      if (signal.aborted) throw transportError("aborted", "Cancelled");
      try {
        const post = await legacySocial.postStatus(toCreateStatus(req));
        return { postId: post.postId };
      } catch (e) { throw mapError(e, "create"); }
    },

    async findCreatedStatus(req, since) {
      let feed;
      try { feed = await legacySocial.feed(); } catch (e) { throw mapError(e, "read"); }
      return feed.mine
        .filter((p) => Date.parse(p.createdAt) >= Date.parse(since) && p.kind === req.kind)
        .filter((p) => {
          if (req.kind === "TEXT") return p.text === req.text && (p.background ?? null) === (req.background ?? null);
          if (p.media?.mediaId !== req.mediaId) return false;
          return req.kind !== "VIDEO" || (p.startMs === req.startMs && p.endMs === req.endMs);
        })
        .sort((a, b) => Date.parse(a.createdAt) - Date.parse(b.createdAt))
        .map((p) => ({ postId: p.postId }));
    },
  };
}
