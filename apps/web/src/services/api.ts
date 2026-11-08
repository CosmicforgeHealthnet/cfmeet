import axios from "axios";
import { attachAccountSession } from "@/lib/auth/axios-session";

// Public API instance (no auth interceptors)
export const publicApi = axios.create({
  baseURL: process.env.NEXT_PUBLIC_API_URL || "http://localhost:3001/api/v1",
  headers: {
    "Content-Type": "application/json",
  },
});

const api = axios.create({
  baseURL: process.env.NEXT_PUBLIC_API_URL || "http://localhost:3001/api/v1",
  headers: {
    "Content-Type": "application/json",
  },
});

// In-memory access token on every request; one refresh-and-retry on 401 through the
// httpOnly session cookie (src/lib/auth/session-core.ts, ACC-019).
attachAccountSession(api);

export default api;
