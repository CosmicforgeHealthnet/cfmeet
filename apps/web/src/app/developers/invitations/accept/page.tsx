"use client";

import { Suspense } from "react";
import AcceptInvitationPage from "@/components/developer-portal/pages/AcceptInvitationPage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <AcceptInvitationPage />
    </Suspense>
  );
}
