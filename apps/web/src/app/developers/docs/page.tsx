"use client";

import { Suspense } from "react";
import DocsPage from "@/components/developer-portal/pages/DocsPage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <DocsPage />
    </Suspense>
  );
}
