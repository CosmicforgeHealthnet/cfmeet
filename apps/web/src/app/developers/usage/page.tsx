"use client";

import { Suspense } from "react";
import UsagePage from "@/components/developer-portal/pages/UsagePage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <UsagePage />
    </Suspense>
  );
}
