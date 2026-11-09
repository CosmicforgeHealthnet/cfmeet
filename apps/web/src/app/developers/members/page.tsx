"use client";

import { Suspense } from "react";
import MembersPage from "@/components/developer-portal/pages/MembersPage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <MembersPage />
    </Suspense>
  );
}
