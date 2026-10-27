import axios from "axios";
import type { CallingErrorEnvelope } from "@/types/calling";

export function callingErrorMessage(error: unknown): string {
  if (axios.isAxiosError<CallingErrorEnvelope>(error)) {
    const code = error.response?.data?.error?.code;
    if (code === "STALE_CALL_VERSION") return "The call changed. Its latest state has been refreshed; try again.";
    if (code === "ANSWERED_ELSEWHERE") return "This call was answered on another device.";
    if (code === "CALLING_NOT_CONFIGURED") return "Calling has not been enabled for this account yet.";
    return error.response?.data?.error?.message || "Could not reach calling. Please try again.";
  }
  return error instanceof Error ? error.message : "Calling is temporarily unavailable.";
}
