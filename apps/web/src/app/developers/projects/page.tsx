"use client";

import { Suspense } from "react";
import ProjectsPage from "@/components/developer-portal/pages/ProjectsPage";
import { Loading } from "@/components/developer-portal/ui";

export default function Page() {
  return (
    <Suspense fallback={<Loading />}>
      <ProjectsPage />
    </Suspense>
  );
}
