/**
 * TRANSITIONAL (one release): routes the publishing queue between the Messaging transport and
 * the previous `/api/v2/social/*` one (see social-backend.ts and social-transport.ts).
 * TODO(#193 / ADR 0069 "Rollout"): delete after #193 is deployed.
 * Type-only imports: unit-tested under node --experimental-strip-types.
 */
import type { BackendSwitch } from "../../services/v2/social-backend";
import type { StatusTransport, TransportError } from "./types";

export function createSwitchingTransport(modern: StatusTransport, legacy: StatusTransport, backend: BackendSwitch): StatusTransport {
  const owner = new Map<string, StatusTransport>();
  const current = () => (backend.current() === "legacy" ? legacy : modern);
  const of = (uploadId: string) => owner.get(uploadId) ?? current();
  return {
    get capabilities() { return current().capabilities; },
    startUpload: (req, signal) => backend.run(
      async () => { const u = await modern.startUpload(req, signal); owner.set(u.uploadId, modern); return u; },
      async () => { const u = await legacy.startUpload(req, signal); owner.set(u.uploadId, legacy); return u; },
    ),
    storedParts: (uploadId, signal) => of(uploadId).storedParts(uploadId, signal),
    uploadPart: (uploadId, index, body, signal, onProgress) => of(uploadId).uploadPart(uploadId, index, body, signal, onProgress),
    completeUpload: (uploadId, signal) => of(uploadId).completeUpload(uploadId, signal),
    cancelUpload: (uploadId) => of(uploadId).cancelUpload(uploadId),
    createStatus: (req, idempotencyKey, signal) => {
      // Media is posted where it was uploaded.
      const media = "mediaId" in req ? owner.get(req.mediaId) : undefined;
      if (media) return media.createStatus(req, idempotencyKey, signal);
      return backend.run(() => modern.createStatus(req, idempotencyKey, signal), () => legacy.createStatus(req, idempotencyKey, signal));
    },
    findCreatedStatus: async (req, since, signal) => {
      const t = current();
      if (!t.findCreatedStatus) throw { transportError: true, kind: "rejected", message: "Cannot check created statuses" } satisfies TransportError;
      return t.findCreatedStatus(req, since, signal);
    },
  };
}
