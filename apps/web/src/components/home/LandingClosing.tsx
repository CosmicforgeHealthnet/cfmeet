import Image from "next/image";
import Link from "next/link";
import { Globe, Laptop, ShieldCheck, Smartphone, Tablet, UserCheck, Video } from "lucide-react";
import Reveal from "./Reveal";

const safety = [
  {
    icon: ShieldCheck,
    title: "You control who joins",
    body: "Waiting rooms, passcodes and host controls keep every meeting in the right hands.",
  },
  {
    icon: Video,
    title: "Clear recording consent",
    body: "Everyone is told and asked before a meeting is recorded, and sees when recording is on.",
  },
  {
    icon: UserCheck,
    title: "Your account, your devices",
    body: "See where you're signed in and sign out any session in one tap.",
  },
];

export function LandingSafety() {
  return (
    <section aria-labelledby="safety-heading" className="border-t border-border bg-muted/30 py-14 sm:py-20 dark:bg-[#18181A]">
      <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="text-sm font-semibold uppercase tracking-[0.18em] text-primary">Safety and privacy</p>
          <h2 id="safety-heading" className="mt-3 text-3xl font-bold tracking-tight text-foreground sm:text-5xl">
            Your safety and privacy matter.
          </h2>
        </Reveal>
        <div className="mt-12 grid gap-5 md:grid-cols-3 lg:gap-6">
          {safety.map(({ icon: Icon, title, body }, i) => (
            <Reveal key={title} delay={i * 100} className="flex">
              <div className="w-full rounded-3xl border border-border bg-card p-7 transition-shadow hover:shadow-lg">
                <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary">
                  <Icon className="size-6" aria-hidden />
                </span>
                <h3 className="mt-5 text-lg font-semibold text-foreground">{title}</h3>
                <p className="mt-2 text-base leading-relaxed text-muted-foreground">{body}</p>
              </div>
            </Reveal>
          ))}
        </div>
      </div>
    </section>
  );
}

const devices = [
  { icon: Laptop, label: "Computer" },
  { icon: Tablet, label: "Tablet" },
  { icon: Smartphone, label: "Phone" },
];

export function LandingEverywhere() {
  return (
    <section aria-labelledby="everywhere-heading" className="pb-14 sm:pb-20">
      <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
        <Reveal className="grid items-center gap-8 rounded-[28px] border border-border bg-muted/40 px-6 py-10 sm:px-10 lg:grid-cols-2 lg:px-14 lg:py-14">
          <div>
            <h2 id="everywhere-heading" className="text-3xl font-bold tracking-tight text-foreground sm:text-4xl">
              Use it in your browser or on your phone.
            </h2>
            <p className="mt-4 max-w-md text-base leading-relaxed text-muted-foreground sm:text-lg">
              Nothing to install to get started. Open CF Meet on any device and pick up where you left off.
            </p>
            <p className="mt-6 flex items-center gap-2 text-sm text-muted-foreground">
              <Globe className="size-4 text-primary" /> Works in Chrome, Edge, Safari and Firefox
            </p>
          </div>
          <ul className="grid grid-cols-3 gap-3 sm:gap-4">
            {devices.map(({ icon: Icon, label }) => (
              <li
                key={label}
                className="flex flex-col items-center gap-3 rounded-2xl border border-border bg-card px-3 py-6 text-sm font-medium text-foreground sm:py-8"
              >
                <Icon className="size-8 text-primary" aria-hidden />
                {label}
              </li>
            ))}
          </ul>
        </Reveal>
      </div>
    </section>
  );
}

export function LandingCta() {
  return (
    <section aria-labelledby="cta-heading" className="px-4 pb-14 sm:px-6 sm:pb-20 lg:px-8">
      <Reveal className="relative mx-auto max-w-7xl overflow-hidden rounded-[28px] bg-primary px-6 py-12 dark:bg-[#0B57D0] text-center sm:px-12 sm:py-16">
        <div
          aria-hidden
          className="absolute inset-0"
          style={{
            backgroundImage:
              "linear-gradient(rgba(255,255,255,0.2) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.2) 1px, transparent 1px)",
            backgroundSize: "40px 40px",
            maskImage: "radial-gradient(ellipse 70% 90% at 100% 0%, black 10%, transparent 70%)",
            WebkitMaskImage: "radial-gradient(ellipse 70% 90% at 100% 0%, black 10%, transparent 70%)",
          }}
        />
        <h2 id="cta-heading" className="relative text-3xl font-bold tracking-tight text-white sm:text-5xl">
          Start connecting with CF Meet today.
        </h2>
        <p className="relative mx-auto mt-4 max-w-xl text-base text-white/80 sm:text-lg">
          Create your free account in a minute, or join a meeting as a guest.
        </p>
        <div className="relative mt-8 flex flex-col items-center justify-center gap-3 sm:flex-row">
          <Link
            href="/signup"
            className="inline-flex h-12 w-full items-center justify-center rounded-xl bg-white px-7 font-semibold text-primary dark:text-[#0B57D0] transition-colors hover:bg-white/90 sm:w-auto"
          >
            Create free account
          </Link>
          <Link
            href="/join"
            className="inline-flex h-12 w-full items-center justify-center rounded-xl border border-white/50 px-7 font-semibold text-white transition-colors hover:bg-white/10 sm:w-auto"
          >
            Join a meeting
          </Link>
        </div>
      </Reveal>
    </section>
  );
}

const footerLinks = [
  {
    title: "Product",
    links: [
      { label: "Meetings", href: "#meetings" },
      { label: "Calls", href: "#calls" },
      { label: "Messages", href: "#messages" },
      { label: "Contacts", href: "#contacts" },
    ],
  },
  {
    title: "Get started",
    links: [
      { label: "Create account", href: "/signup" },
      { label: "Sign in", href: "/login" },
      { label: "Join a meeting", href: "/join" },
    ],
  },
  {
    title: "More",
    links: [
      { label: "Organizations", href: "#organizations" },
      { label: "Developers", href: "/developers" },
    ],
  },
];

export function LandingFooter() {
  return (
    <footer className="border-t border-border bg-muted/50">
      <div className="mx-auto max-w-7xl px-4 py-12 sm:px-6 lg:px-8">
        <div className="grid grid-cols-2 gap-10 md:grid-cols-4">
          <div className="col-span-2 md:col-span-1">
            <Image src="/light-mode-cf-meet.png" alt="CF Meet" width={120} height={40} className="h-8 w-auto dark:hidden" />
            <Image src="/dark-mode-cfmeet.png" alt="CF Meet" width={120} height={40} className="hidden h-8 w-auto dark:block" />
            <p className="mt-4 max-w-xs text-sm leading-relaxed text-muted-foreground">
              Meet, call and message, all in one place.
            </p>
          </div>
          {footerLinks.map((col) => (
            <div key={col.title}>
              <p className="text-sm font-semibold text-foreground">{col.title}</p>
              <ul className="mt-4 space-y-3">
                {col.links.map((l) => (
                  <li key={l.label}>
                    <Link href={l.href} className="text-sm text-muted-foreground transition-colors hover:text-foreground">
                      {l.label}
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
        <div className="mt-12 flex flex-col gap-4 border-t border-border pt-6 text-sm text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
          <p>© 2026 CosmicForge Tech Limited. All rights reserved.</p>
          <div className="flex gap-6">
            <Link href="/terms" className="hover:text-foreground">Terms of Service</Link>
            <Link href="/privacy" className="hover:text-foreground">Privacy Policy</Link>
          </div>
        </div>
      </div>
    </footer>
  );
}
