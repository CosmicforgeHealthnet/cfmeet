"use client";

import { Suspense } from "react";
import WorkspacePage from "@/components/developer-portal/pages/WorkspacePage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <WorkspacePage />
    </Suspense>
  );
}
