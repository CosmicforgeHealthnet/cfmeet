"use client";

import { Suspense } from "react";
import OverviewPage from "@/components/developer-portal/pages/OverviewPage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <OverviewPage />
    </Suspense>
  );
}
