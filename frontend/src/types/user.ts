export interface UserProfile {
  id: string;
  email: string;
  username: string;
  role: string;
  status: string;
  created_at: string;
}

export interface UpdateProfileRequest {
  username?: string;
}
