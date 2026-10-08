import axios, { AxiosError, InternalAxiosRequestConfig } from "axios";
import { API_URL } from "@/lib/constants";
import { tokenStorage, clearAllStorage } from "@/lib/storage/secure-store";
import { router } from "expo-router";

interface RefreshTokenResponse {
  access_token: string;
  refresh_token: string;
}

interface QueueItem {
  resolve: (value: unknown) => void;
  reject: (error: unknown) => void;
}

let isRefreshing = false;
let failedQueue: QueueItem[] = [];

const processQueue = (error: Error | null, token: string | null = null): void => {
  failedQueue.forEach((prom) => {
    if (error) {
      prom.reject(error);
    } else {
      prom.resolve(token);
    }
  });
  failedQueue = [];
};

export const api = axios.create({
  baseURL: API_URL,
  headers: {
    "Content-Type": "application/json",
  },
  timeout: 30000,
});

export const publicApi = axios.create({
  baseURL: API_URL,
  headers: {
    "Content-Type": "application/json",
  },
  timeout: 30000,
});

api.interceptors.request.use(
  async (config: InternalAxiosRequestConfig) => {
    const token = await tokenStorage.getAccessToken();
    if (token && config.headers) {
      config.headers.Authorization = `Bearer ${token}`;
    }
    return config;
  },
  (error: AxiosError) => {
    return Promise.reject(error);
  }
);

api.interceptors.response.use(
  (response) => response,
  async (error: AxiosError) => {
    const originalRequest = error.config as InternalAxiosRequestConfig & {
      _retry?: boolean;
    };

    if (!originalRequest) {
      return Promise.reject(error);
    }

    const isAuthEndpoint = originalRequest.url?.includes("/auth/");

    if (error.response?.status === 401 && !originalRequest._retry && !isAuthEndpoint) {
      if (isRefreshing) {
        return new Promise((resolve, reject) => {
          failedQueue.push({ resolve, reject });
        })
          .then((token) => {
            if (originalRequest.headers) {
              originalRequest.headers.Authorization = `Bearer ${token}`;
            }
            return api(originalRequest);
          })
          .catch((err) => {
            return Promise.reject(err);
          });
      }

      originalRequest._retry = true;
      isRefreshing = true;

      try {
        const refreshToken = await tokenStorage.getRefreshToken();

        if (!refreshToken) {
          throw new Error("No refresh token available");
        }

        const response = await publicApi.post<RefreshTokenResponse>("/auth/refresh", {
          refresh_token: refreshToken,
        });

        const { access_token, refresh_token } = response.data;
        await tokenStorage.setTokens(access_token, refresh_token);

        processQueue(null, access_token);

        if (originalRequest.headers) {
          originalRequest.headers.Authorization = `Bearer ${access_token}`;
        }

        return api(originalRequest);
      } catch (refreshError) {
        processQueue(refreshError as Error, null);
        await clearAllStorage();
        router.replace("/login");
        return Promise.reject(refreshError);
      } finally {
        isRefreshing = false;
      }
    }

    return Promise.reject(error);
  }
);

export interface ApiError {
  message: string;
  status: number;
  errors?: Record<string, string[]>;
}

const getStatusMessage = (status: number): string => {
  switch (status) {
    case 400:
      return "Invalid request. Please check your input.";
    case 401:
      return "Invalid email or password.";
    case 403:
      return "You don't have permission to perform this action.";
    case 404:
      return "The requested resource was not found.";
    case 429:
      return "Too many requests. Please wait a moment and try again.";
    case 500:
      return "Server error. Please try again later.";
    case 502:
    case 503:
    case 504:
      return "Service temporarily unavailable. Please try again later.";
    default:
      return "An error occurred. Please try again.";
  }
};

export const getApiError = (error: unknown): ApiError => {
  if (axios.isAxiosError(error)) {
    const axiosError = error as AxiosError<{ message?: string; errors?: Record<string, string[]> }>;
    const status = axiosError.response?.status || 500;
    const serverMessage = axiosError.response?.data?.message;

    return {
      message: serverMessage || getStatusMessage(status),
      status,
      errors: axiosError.response?.data?.errors,
    };
  }
  return {
    message: "An unexpected error occurred",
    status: 500,
  };
};
