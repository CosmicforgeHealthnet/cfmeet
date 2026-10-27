import callingApi from "./api";
import type { Endpoint, EndpointList, RegisterEndpointRequest, SignalingCredential, SignalingCredentialRequest } from "@/types/calling";

export const endpointsService = {
  identity: async () => (await callingApi.get<{ principalId: string }>("/identity")).data,
  list: async () => (await callingApi.get<EndpointList>("/v1/calling/endpoints")).data,
  register: async (body: RegisterEndpointRequest) =>
    (await callingApi.post<Endpoint>("/v1/calling/endpoints", body)).data,
  revoke: async (id: string) =>
    (await callingApi.delete<Endpoint>(`/v1/calling/endpoints/${encodeURIComponent(id)}`)).data,
  mintSignalingCredential: async (body: SignalingCredentialRequest) =>
    (await callingApi.post<SignalingCredential>("/v1/calling/signaling-credentials", body)).data,
};
