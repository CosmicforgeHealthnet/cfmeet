import axios from "axios";
import { CALLING_BASE_URL } from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import type { AcceptResult, CallHistoryFilters, CallHistoryList, CommandRequest, DeclineResult, MediaCredential, RecoveredInvitationList, SignalingCredential } from "@/types/calling";

/** Each client owns its endpoint credential; no module-global credential races. */
export function createSignalingService(credential: SignalingCredential) {
  const api = axios.create({ baseURL: "/api/calling-signaling", timeout: 20_000,
    headers: { Authorization: `Bearer ${credential.token}`, "Content-Type": "application/json" } });
  return {
    invitations: async () => (await api.get<RecoveredInvitationList>("/v1/calling/invitations")).data,
    history: async (filters: CallHistoryFilters = {}) =>
      (await api.get<CallHistoryList>("/v1/calling/history", { params: filters })).data,
    accept: async (id: string, body: CommandRequest, key = newIdempotencyKey()) =>
      (await api.post<AcceptResult>(`/v1/calling/invitations/${encodeURIComponent(id)}/accept`, body, { headers: { "Idempotency-Key": key } })).data,
    decline: async (id: string, body: CommandRequest, key = newIdempotencyKey()) =>
      (await api.post<DeclineResult>(`/v1/calling/invitations/${encodeURIComponent(id)}/decline`, body, { headers: { "Idempotency-Key": key } })).data,
    mediaCredential: async (id: string) =>
      (await api.post<MediaCredential>(`/v1/calling/sessions/${encodeURIComponent(id)}/media-credentials`, {})).data,
    connect: () => {
      const url = new URL(credential.signalingUrl, CALLING_BASE_URL);
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      return new WebSocket(url, ["cfmeet.signaling.v1", `bearer.${credential.token}`]);
    },
  };
}
