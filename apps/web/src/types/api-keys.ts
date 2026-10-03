export interface ApiKey {
  id: string;
  api_key_masked: string;
  usage_limit: number;
  used_count: number;
  expires_at: string;
  status: "active" | "revoked";
  created_at: string;
  updated_at: string;
}

export interface CreateApiKeyResponse {
  id: string;
  api_key: string; // The full key, only returned on creation
  usage_limit: number;
  used_count: number;
  expires_at: string;
  status: string;
  created_at: string;
}

export interface ListApiKeysResponse {
  success: boolean;
  data: ApiKey[];
}

export interface CreateApiKeyApiResponse {
  success: boolean;
  data: CreateApiKeyResponse;
}

export interface RevokeApiKeyResponse {
  success: boolean;
  message: string;
}
