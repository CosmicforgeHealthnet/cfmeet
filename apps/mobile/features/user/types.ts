export interface User {
  id: string;
  email: string;
  username: string;
  role: string;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface UpdateUserRequest {
  username?: string;
  email?: string;
}

export interface UpdateUserResponse {
  user: User;
}
