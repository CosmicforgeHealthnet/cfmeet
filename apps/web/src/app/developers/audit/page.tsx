"use client";

import { Suspense } from "react";
import AuditPage from "@/components/developer-portal/pages/AuditPage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <AuditPage />
    </Suspense>
  );
}
