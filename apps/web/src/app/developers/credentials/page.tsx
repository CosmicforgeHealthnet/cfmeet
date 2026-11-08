"use client";

import { Suspense } from "react";
import CredentialsPage from "@/components/developer-portal/pages/CredentialsPage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <CredentialsPage />
    </Suspense>
  );
}
