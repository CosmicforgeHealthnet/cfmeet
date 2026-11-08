"use client";

import { Suspense } from "react";
import WebhooksPage from "@/components/developer-portal/pages/WebhooksPage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <WebhooksPage />
    </Suspense>
  );
}
