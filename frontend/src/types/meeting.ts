export interface Meeting {
  id: string;
  meeting_identifier: string;
  host_id: string;
  title: string;
  metadata: string;
  is_private: boolean;
  start_time: string;
  end_time: string;
  status: MeetingStatus;
  join_url: string;
  created_at: string;
  updated_at: string;
}

export type MeetingStatus = "scheduled" | "live" | "ended" | "cancelled";

export interface MeetingsParams {
  page?: number;
  limit?: number;
  status?: MeetingStatus;
}

export interface CreateMeetingRequest {
  title: string;
  start_time: string;
  end_time: string;
  timezone: string;
  is_private: boolean;
  metadata?: string;
}

export interface CreateMeetingResponse {
  success: boolean;
  data: Meeting;
}

export interface InstantMeetingRequest {
  title?: string;
}

export interface InstantMeetingData {
  id: string;
  meeting_identifier: string;
  host_id: string;
  title: string;
  is_private: boolean;
  start_time: string;
  end_time: string;
  status: string;
  join_url: string;
  created_at: string;
  participant_id: string;
  join_token: string;
  livekit_url: string;
  room_name: string;
}

export interface InstantMeetingResponse {
  success: boolean;
  data: InstantMeetingData;
}

export interface MeetingResponse {
  success: boolean;
  data: Meeting;
}

export interface UpdateMeetingRequest {
  title?: string;
  start_time?: string;
  end_time?: string;
  timezone?: string;
  is_private?: boolean;
  metadata?: string;
}

export interface JoinMeetingData {
  meeting_id: string;
  participant_id: string;
  role: "host" | "participant" | "viewer";
  join_token: string;
  livekit_url: string;
  room_name: string;
  access_token?: string;
  refresh_token?: string;
}

export interface JoinMeetingResponse {
  success: boolean;
  data: JoinMeetingData;
}

export interface JoinMeetingRequest {
  user_id?: string;
  display_name: string;
}

export interface WaitingParticipant {
  participant_id: string;
  user_id?: string;
  display_name: string;
  join_time: string;
}

export interface WaitingListResponse {
  success: boolean;
  data: WaitingParticipant[];
}

export interface ParticipantActionResponse {
  success: boolean;
  message: string;
}

export interface Participant {
  participant_id: string;
  meeting_id: string;
  user_id?: string;
  role: string;
  display_name: string;
  status: string;
  join_time: string;
  leave_time?: string;
  is_muted: boolean;
  is_video_on: boolean;
  is_screen_sharing: boolean;
}

export interface ParticipantsListResponse {
  success: boolean;
  data: Participant[];
}

export interface MeetingsResponse {
  success: boolean;
  data: Meeting[];
  meta: {
    page: number;
    limit: number;
    total: number;
    total_pages: number;
  };
}

export interface ChatMessage {
  id: string;
  participant_id: string;
  display_name: string;
  message: string;
  created_at: string;
}
