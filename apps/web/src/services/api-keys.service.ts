import api from "./api";
import {
  ListApiKeysResponse,
  CreateApiKeyApiResponse,
  RevokeApiKeyResponse,
} from "@/types/api-keys";

export const apiKeysService = {
  getAll: async () => {
    const response = await api.get<ListApiKeysResponse>("/api-keys");
    return response.data;
  },

  create: async () => {
    const response = await api.post<CreateApiKeyApiResponse>("/api-keys", {});
    return response.data;
  },

  revoke: async (id: string) => {
    const response = await api.delete<RevokeApiKeyResponse>(`/api-keys/${id}`);
    return response.data;
  },
};
