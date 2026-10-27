import { Suspense } from "react";
import ConferencesContent from "@/components/conferences/ConferencesContent";

export default function ConferencesPage() {
  return <Suspense fallback={<p role="status">Loading conferences…</p>}><ConferencesContent /></Suspense>;
}
