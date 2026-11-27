/**
 * TEMPORARY TRANSPORT — swap point for the Messaging-backed upload API.
 *
 * Adapts today's Meeting Service status API (`/social/media`, its chunks and `complete`,
 * `/social/status`, the feed) to the queue's `StatusTransport`. The integration step replaces
 * this file (or the `createSocialTransport()` call in client.ts) with a transport over the new
 * `social.service.ts` API; nothing else in the queue changes.
 *
 * Where today's API lacks a capability the adapter degrades safely, and says so in
 * `capabilities`:
 *  - DEGRADED listParts: there is no "which parts do you have" call. After a reload the queue
 *    trusts the parts it recorded (a re-sent piece replaces the stored one, so that is safe); if
 *    `complete` then reports missing pieces (400) or the upload is gone (404) the upload is
 *    restarted — that upload only.
 *  - DEGRADED idempotentCreate: POST /social/status takes no idempotency key. After an
 *    ambiguous create failure the queue calls `findCreatedStatus`, which looks for a matching
 *    status (same media id / range, or same text) in the feed's `mine` list before re-creating.
 *  - DEGRADED cancelUpload: there is no endpoint to discard an unfinished upload; it is left to
 *    expire server-side (it is never visible to anyone: statuses only reference complete media).
 *  - groupId / sequence are not sent (today's API would reject unknown fields); the queue's own
 *    ordering guarantees the order.
 */
import axios from "axios";
import apiV2 from "@/services/v2/api";
import { socialService, type CreateStatus, type MediaView } from "@/services/v2/social.service";
import { transportError } from "./queue";
import type { StatusCreateRequest, StatusTransport, TransportError } from "./types";

const id = (s: string) => encodeURIComponent(s);

function messageOf(e: unknown): string | undefined {
  const data = (e as { response?: { data?: { error?: { message?: string }; message?: string } } })?.response?.data;
  return data?.error?.message ?? data?.message;
}

/** Axios error → the queue's error kinds. `phase` decides what 400/404/409 mean. */
function mapError(e: unknown, phase: "start" | "part" | "complete" | "create" | "read"): TransportError {
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

export function createSocialTransport(): StatusTransport {
  return {
    capabilities: { listParts: false, idempotentCreate: false, cancelUpload: false },

    async startUpload(req, signal) {
      try {
        const m = (await apiV2.post<MediaView>("/social/media", {
          purpose: "STATUS", kind: req.kind, contentType: req.contentType, sizeBytes: req.sizeBytes,
          durationMs: req.durationMs === null ? undefined : Math.round(req.durationMs),
        }, { signal })).data;
        return { uploadId: m.mediaId, partSize: m.chunkBytes, partCount: m.chunkCount };
      } catch (e) { throw mapError(e, "start"); }
    },

    async storedParts() {
      return null; // DEGRADED: no part listing today.
    },

    async uploadPart(uploadId, index, body, signal, onProgress) {
      try {
        const bytes = new Uint8Array(await body.arrayBuffer());
        await apiV2.put(`/social/media/${id(uploadId)}/chunks/${index}`, bytes, {
          signal,
          headers: { "Content-Type": "application/octet-stream" },
          transformRequest: [(d) => d],
          onUploadProgress: (p) => onProgress?.(p.loaded),
        });
      } catch (e) { throw mapError(e, "part"); }
    },

    async completeUpload(uploadId, signal) {
      try {
        const m = (await apiV2.post<MediaView>(`/social/media/${id(uploadId)}/complete`, {}, { signal })).data;
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
        const post = await socialService.postStatus(toCreateStatus(req));
        return { postId: post.postId };
      } catch (e) { throw mapError(e, "create"); }
    },

    async findCreatedStatus(req, since) {
      let feed;
      try { feed = await socialService.feed(); } catch (e) { throw mapError(e, "read"); }
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
