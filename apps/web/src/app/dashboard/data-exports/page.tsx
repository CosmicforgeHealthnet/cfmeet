"use client";

import MessagingExportDeliveries from "@/components/account/MessagingExportDeliveries";

/** ADR 0068: owner-delivered Messaging exports waiting for the signed-in person. */
export default function DataExportsPage() {
  return <MessagingExportDeliveries />;
}
