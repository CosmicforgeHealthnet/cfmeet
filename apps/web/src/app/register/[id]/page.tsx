import { Suspense } from "react";
import PublicRegistrationForm from "@/components/meeting-spaces/PublicRegistrationForm";

export default function RegistrationPage() {
  return <Suspense fallback={<main className="p-6"><p role="status">Loading registration…</p></main>}>
    <PublicRegistrationForm />
  </Suspense>;
}
