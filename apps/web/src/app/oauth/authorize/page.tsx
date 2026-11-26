import type { Metadata } from "next";
import OAuthConsent from "@/components/apps/OAuthConsent";

export const metadata: Metadata = {
  title: "Connect an app - CF Meet",
  // The request carries a PKCE challenge and state: never indexed, never in a referrer.
  robots: { index: false, follow: false },
  referrer: "no-referrer",
};

export default function OAuthAuthorizePage() {
  return <OAuthConsent />;
}
