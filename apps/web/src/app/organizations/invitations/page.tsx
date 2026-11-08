import type { Metadata } from "next";
import OrganizationInvitationDecision from "@/components/organizations/OrganizationInvitationDecision";

export const metadata: Metadata = {
  title: "Organization invitation - CF Meet",
  // The token is only in the fragment, but never let this page be indexed or leak a referrer.
  robots: { index: false, follow: false },
  referrer: "no-referrer",
};

export default function OrganizationInvitationPage() {
  return <OrganizationInvitationDecision />;
}
