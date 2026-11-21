/**
 * ADR 0068 — owner-delivered exports (see src/lib/account/owner-exports.ts). Tokens and
 * download links pass through here in memory only: never logged, cached or stored.
 */
import apiV2 from "./v2/api";
import type { ExportDelivery } from "@/lib/account/owner-exports";

/** The raw JSON text of a downloaded document (kept as text so it is saved unchanged). */
const asText = { responseType: "text" as const, transformResponse: (data: unknown) => data };

export interface ExportClaimGrant {
  exportId: string;
  deliveryRef: string;
  method: "GET";
  url: string;
  expiresAt: string;
  size: number | null;
  sha256: string | null;
}

export const ownerExportsService = {
  /** The account export CF Meet support requested: `GET /me/data-export?exportId` + token header. */
  downloadAccountExport: async (exportId: string, token: string): Promise<string> =>
    (
      await apiV2.get<string>("/me/data-export", {
        ...asText,
        params: { exportId },
        headers: { "X-CF-Data-Export-Token": token },
      })
    ).data,

  /** Messaging exports waiting for the signed-in recipient. */
  listMessagingExports: async (): Promise<ExportDelivery[]> =>
    (await apiV2.get<{ items?: ExportDelivery[] }>("/messaging/export-deliveries")).data.items ?? [],

  /** The single claim of a Messaging export: one short-lived download link. */
  claimMessagingExport: async (exportId: string): Promise<ExportClaimGrant> =>
    (await apiV2.post<ExportClaimGrant>(`/messaging/exports/${encodeURIComponent(exportId)}/claim`, {})).data,

  /** The organization export of an emailed link (OWNER, signed in). */
  downloadOrganizationExport: async (organizationId: string, exportId: string, token: string): Promise<string> =>
    (
      await apiV2.post<string>(
        `/organizations/${encodeURIComponent(organizationId)}/exports/${encodeURIComponent(exportId)}/download`,
        { token },
        asText,
      )
    ).data,
};
