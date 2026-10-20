/**
 * Extract error message from API error responses
 */

import { AxiosError } from "axios";

interface ApiErrorResponse {
  error?: {
    message?: string;
    code?: string;
  };
  message?: string;
}

/**
 * Extracts a user-friendly error message from various error types
 * Handles Axios errors, standard Error objects, and unknown errors
 */
export function getErrorMessage(err: unknown, fallback = "An error occurred"): string {
  // Handle Axios errors with API response
  if (err && typeof err === "object" && "response" in err) {
    const axiosError = err as AxiosError<ApiErrorResponse>;
    const data = axiosError.response?.data;

    // Try to get message from error.message (V2 API format)
    if (data?.error?.message) {
      return data.error.message;
    }

    // Try to get message from root message (V1 API format)
    if (data?.message) {
      return data.message;
    }

    // Fall back to axios error message
    if (axiosError.message) {
      return axiosError.message;
    }
  }

  // Handle standard Error objects
  if (err instanceof Error) {
    return err.message;
  }

  // Handle string errors
  if (typeof err === "string") {
    return err;
  }

  return fallback;
}
