import api from "./api";
import type {
  Webhook,
  CreateWebhookRequest,
  CreateWebhookResponse,
  UpdateWebhookRequest,
  WebhookApiResponse,
  CreateWebhookApiResponse,
  WebhooksListResponse,
} from "@/types/webhook";

export const webhookService = {
  getWebhooks: async (): Promise<Webhook[]> => {
    const response = await api.get<WebhooksListResponse>("/webhooks");
    return response.data.data;
  },

  createWebhook: async (data: CreateWebhookRequest): Promise<CreateWebhookResponse> => {
    const response = await api.post<CreateWebhookApiResponse>("/webhooks", data);
    return response.data.data;
  },

  updateWebhook: async (id: string, data: UpdateWebhookRequest): Promise<Webhook> => {
    const response = await api.patch<WebhookApiResponse>(`/webhooks/${id}`, data);
    return response.data.data;
  },

  deleteWebhook: async (id: string): Promise<void> => {
    await api.delete(`/webhooks/${id}`);
  },
};
