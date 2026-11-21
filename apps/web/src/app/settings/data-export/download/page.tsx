import type { Metadata } from "next";
import AccountExportDownload from "@/components/account/AccountExportDownload";

export const metadata: Metadata = {
  title: "Download your data - CF Meet",
  // The link carries a single-use token: never index this page or leak a referrer.
  robots: { index: false, follow: false },
  referrer: "no-referrer",
};

export default function AccountExportDownloadPage() {
  return <AccountExportDownload />;
}
