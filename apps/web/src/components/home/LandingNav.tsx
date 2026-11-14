"use client";

import { useEffect, useState } from "react";
import Image from "next/image";
import Link from "next/link";
import { Menu, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ThemeToggle } from "@/components/ui/theme-toggle";
import { storageStore } from "@/store";
import { accountSession } from "@/lib/auth/session";

const links = [
  { label: "Product", href: "#product" },
  { label: "Organizations", href: "#organizations" },
  { label: "Developers", href: "#developers" },
];

export default function LandingNav() {
  const [menuOpen, setMenuOpen] = useState(false);
  // Read after mount (storage only exists in the browser) so the server render matches.
  const [signedIn, setSignedIn] = useState(false);
  useEffect(() => { setSignedIn(!!storageStore.getUser() && accountSession.hasSession()); }, []);

  return (
    <header className="sticky top-0 z-40 border-b border-border/60 bg-background/80 backdrop-blur-md">
      <nav
        aria-label="Main"
        className="mx-auto flex h-16 max-w-7xl items-center justify-between gap-4 px-4 sm:px-6 lg:px-8"
      >
        <Link href="/" className="flex shrink-0 items-center" aria-label="CF Meet home">
          <Image
            src="/light-mode-cf-meet.png"
            alt="CF Meet"
            width={120}
            height={40}
            className="h-8 w-auto dark:hidden"
            priority
          />
          <Image
            src="/dark-mode-cfmeet.png"
            alt="CF Meet"
            width={120}
            height={40}
            className="hidden h-8 w-auto dark:block"
            priority
          />
        </Link>

        <ul className="hidden items-center gap-8 md:flex">
          {links.map((link) => (
            <li key={link.href}>
              <a
                href={link.href}
                className="text-sm font-medium text-muted-foreground transition-colors hover:text-foreground"
              >
                {link.label}
              </a>
            </li>
          ))}
        </ul>

        <div className="flex items-center gap-1 sm:gap-2">
          <ThemeToggle />
          {signedIn ? (
            <Button asChild size="sm" className="rounded-xl">
              <Link href="/dashboard">Open CF Meet</Link>
            </Button>
          ) : (
            <>
              <Link href="/login" className="rounded-xl px-3 py-2 text-sm font-semibold text-foreground transition-colors hover:text-primary">
                Sign in
              </Link>
              <Button asChild size="sm" className="hidden rounded-xl sm:inline-flex">
                <Link href="/signup">Create free account</Link>
              </Button>
            </>
          )}
          <button
            type="button"
            className="inline-flex size-10 items-center justify-center rounded-full text-foreground md:hidden"
            aria-label={menuOpen ? "Close menu" : "Open menu"}
            aria-expanded={menuOpen}
            aria-controls="landing-mobile-menu"
            onClick={() => setMenuOpen((open) => !open)}
          >
            {menuOpen ? <X className="size-5" /> : <Menu className="size-5" />}
          </button>
        </div>
      </nav>

      {menuOpen && (
        <div id="landing-mobile-menu" className="border-t border-border bg-background md:hidden">
          <ul className="mx-auto max-w-7xl space-y-1 px-4 py-3 sm:px-6">
            {links.map((link) => (
              <li key={link.href}>
                <a
                  href={link.href}
                  onClick={() => setMenuOpen(false)}
                  className="block rounded-lg px-3 py-3 text-base font-medium text-foreground hover:bg-muted"
                >
                  {link.label}
                </a>
              </li>
            ))}
          </ul>
        </div>
      )}
    </header>
  );
}
