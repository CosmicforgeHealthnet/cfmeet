export interface Webhook {
  id: string;
  user_id: string;
  endpoint_url: string;
  event_type: string;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface CreateWebhookResponse {
  id: string;
  user_id: string;
  endpoint_url: string;
  event_type: string;
  secret: string;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface CreateWebhookRequest {
  endpoint_url: string;
  event_type: string;
}

export interface UpdateWebhookRequest {
  endpoint_url?: string;
  status?: string;
}

export interface WebhookApiResponse {
  success: boolean;
  data: Webhook;
}

export interface CreateWebhookApiResponse {
  success: boolean;
  data: CreateWebhookResponse;
}

export interface WebhooksListResponse {
  success: boolean;
  data: Webhook[];
}

export const WEBHOOK_EVENT_TYPES = [
  { value: "meeting_start", label: "Meeting Start" },
  { value: "meeting_end", label: "Meeting End" },
  { value: "participant_join", label: "Participant Join" },
  { value: "participant_leave", label: "Participant Leave" },
];
