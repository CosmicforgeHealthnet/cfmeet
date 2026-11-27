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
/** "scheduledStartAt" / "time_zone" → "Scheduled start at" / "Time zone". */
function fieldLabel(field?: string): string {
  if (!field || field === "__all__") return "";
  const words = field.split(".").pop()!.replace(/_/g, " ").replace(/([a-z])([A-Z])/g, "$1 $2").toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

export function getErrorMessage(err: unknown, fallback = "An error occurred"): string {
  // Handle Axios errors with API response
  if (err && typeof err === "object" && "response" in err) {
    const axiosError = err as AxiosError<ApiErrorResponse>;
    const data = axiosError.response?.data;

    // Field-level problems (VALIDATION_ERROR details: [{ field, code, message }]): name the field.
    const details = (data?.error as { details?: unknown } | undefined)?.details;
    if (Array.isArray(details) && details.length) {
      const lines = details
        .filter((d): d is { field?: string; message?: string } => !!d && typeof d === "object")
        .map((d) => [fieldLabel(d.field), d.message].filter(Boolean).join(": "))
        .filter(Boolean);
      if (lines.length) return lines.slice(0, 3).join(" · ");
    }

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
