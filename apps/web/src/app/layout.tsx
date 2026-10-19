import type { Metadata } from "next";
import { headers } from "next/headers";
import { Inter } from "next/font/google";
import QueryProvider from "@/providers/QueryProvider";
import ThemeProvider from "@/providers/ThemeProvider";
import "@livekit/components-styles";
import "@livekit/components-styles/prefabs";
import "./globals.css";

const inter = Inter({
  subsets: ["latin"],
  variable: "--font-inter",
});

export const metadata: Metadata = {
  title: "CF Meet - Real-Time Communication Platform",
  description:
    "Secure real-time communication platform with video, audio, and chat capabilities",
  referrer: "strict-origin",
};

export default async function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  // The per-request CSP nonce issued by src/proxy.ts (inline scripts must carry it).
  const nonce = (await headers()).get("x-nonce") ?? undefined;
  return (
    <html lang="en" suppressHydrationWarning>
      <body className={`${inter.variable} font-sans antialiased`}>
        <ThemeProvider nonce={nonce}>
          <QueryProvider>{children}</QueryProvider>
        </ThemeProvider>
      </body>
    </html>
  );
}
