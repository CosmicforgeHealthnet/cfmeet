import { Suspense } from "react";
import InvitationLanding from "@/components/meeting-spaces/InvitationLanding";

export default function InvitationPage() {
  return <Suspense fallback={<p role="status">Loading invitation…</p>}><InvitationLanding /></Suspense>;
}
