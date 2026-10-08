export interface ApiKey {
  id: string;
  api_key_masked: string;
  usage_limit: number;
  used_count: number;
  expires_at: string;
  status: "active" | "revoked" | "expired";
  created_at: string;
}

export interface CreateApiKeyRequest {
  name?: string;
  usage_limit?: number;
  expires_in_days?: number;
}

export interface CreateApiKeyResponse {
  api_key: string;
  id: string;
  message: string;
}

export interface ApiKeysResponse {
  api_keys: ApiKey[];
}

export type WebhookEventType =
  | "meeting_start"
  | "meeting_end"
  | "participant_join"
  | "participant_leave";

export interface Webhook {
  id: string;
  user_id: string;
  endpoint_url: string;
  event_type: WebhookEventType;
  status: "active" | "inactive";
  created_at: string;
  updated_at: string;
}

export interface CreateWebhookRequest {
  endpoint_url: string;
  event_type: WebhookEventType;
}

export interface UpdateWebhookRequest {
  endpoint_url?: string;
  event_type?: WebhookEventType;
  status?: "active" | "inactive";
}

export interface WebhooksResponse {
  webhooks: Webhook[];
}

export const WEBHOOK_EVENT_TYPES: WebhookEventType[] = [
  "meeting_start",
  "meeting_end",
  "participant_join",
  "participant_leave",
];
