import type { Metadata } from "next";
import OrganizationExportDownload from "@/components/organizations/OrganizationExportDownload";

export const metadata: Metadata = {
  title: "Download your organization's data - CF Meet",
  // The token is only in the fragment, but never let this page be indexed or leak a referrer.
  robots: { index: false, follow: false },
  referrer: "no-referrer",
};

export default function OrganizationExportDownloadPage() {
  return <OrganizationExportDownload />;
}
