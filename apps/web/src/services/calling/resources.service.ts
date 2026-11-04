import callingApi from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";

export type CallingResourceMethod = "GET" | "POST" | "PATCH" | "PUT" | "DELETE";

export const callingResourcesService = {
  request: async (
    method: CallingResourceMethod,
    path: string,
    body?: Record<string, unknown>,
  ): Promise<unknown> => {
    const response = await callingApi.request<unknown>({
      method,
      url: path,
      ...(body ? { data: body } : {}),
      ...(["POST", "PATCH", "PUT", "DELETE"].includes(method)
        ? { headers: { "Idempotency-Key": newIdempotencyKey() } }
        : {}),
    });
    return response.data;
  },
};
