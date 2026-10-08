export type MeetingStatus = "scheduled" | "live" | "ended" | "cancelled";

export interface Meeting {
  id: string;
  host_id: string;
  title: string;
  description?: string;
  meeting_identifier: string;
  scheduled_at: string;
  duration_minutes: number;
  is_private: boolean;
  status: MeetingStatus;
  created_at: string;
  updated_at: string;
}

export interface MeetingsResponse {
  meetings: Meeting[];
  total: number;
  page: number;
  limit: number;
  total_pages: number;
}

export interface CreateMeetingRequest {
  title: string;
  description?: string;
  scheduled_at: string;
  duration_minutes: number;
  is_private: boolean;
}

export interface CreateMeetingResponse {
  meeting: Meeting;
}

export interface UpdateMeetingRequest {
  title?: string;
  description?: string;
  scheduled_at?: string;
  duration_minutes?: number;
  is_private?: boolean;
}

export interface InstantMeetingResponse {
  meeting: Meeting;
  join_data: JoinMeetingData;
}

export interface JoinMeetingRequest {
  display_name: string;
  is_audio_enabled?: boolean;
  is_video_enabled?: boolean;
}

export interface JoinMeetingData {
  id: string;
  meeting_identifier: string;
  participant_id: string;
  role: "host" | "participant" | "viewer";
  join_token: string;
  livekit_url: string;
  room_name: string;
}

export interface JoinMeetingResponse {
  message: string;
  data: JoinMeetingData;
  access_token?: string;
}

export interface PublicMeetingInfo {
  id: string;
  title: string;
  host_username: string;
  status: MeetingStatus;
  is_private: boolean;
  scheduled_at: string;
}

export interface Participant {
  id: string;
  meeting_id: string;
  user_id?: string;
  display_name: string;
  role: "host" | "participant" | "viewer";
  is_audio_enabled: boolean;
  is_video_enabled: boolean;
  joined_at: string;
  left_at?: string;
}

export interface ParticipantsResponse {
  participants: Participant[];
}

export interface WaitingParticipant {
  id: string;
  display_name: string;
  requested_at: string;
}

export interface WaitingParticipantsResponse {
  waiting: WaitingParticipant[];
}

export interface ChatMessage {
  id: string;
  participant_id: string;
  display_name: string;
  message: string;
  created_at: string;
}

export interface ChatMessagesResponse {
  messages: ChatMessage[];
}

export interface SendChatMessageRequest {
  message: string;
}

export interface MediaStateRequest {
  enabled: boolean;
}
