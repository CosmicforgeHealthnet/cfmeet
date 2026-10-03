import type { Metadata } from "next";

export const metadata: Metadata = {
  referrer: "no-referrer",
  title: "Reset Password",
  description: "Reset your password securely",
};

export default function ForgotPasswordLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return <>{children}</>;
}
