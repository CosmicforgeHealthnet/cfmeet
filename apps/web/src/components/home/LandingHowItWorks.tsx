import Image from "next/image";
import { Check, Copy, Link2, Mic, PhoneOff, Video } from "lucide-react";
import QrCode from "@/components/communications/QrCode";
import Reveal from "./Reveal";

/** Step 1: a compact sign-up card, with the guest path called out. */
function SignUpScene() {
  return (
    <div className="relative w-full max-w-xs">
      <div className="rounded-2xl border border-border bg-card p-4 shadow-xl shadow-primary/10">
        <p className="text-sm font-semibold text-foreground">Create your account</p>
        <div className="mt-3 rounded-lg border border-input bg-background px-3 py-2 text-xs text-muted-foreground">
          ada@example.com
        </div>
        <div className="mt-2 rounded-lg bg-primary py-2 text-center text-xs font-semibold text-primary-foreground">
          Continue
        </div>
        <div className="my-3 flex items-center gap-2 text-[10px] uppercase tracking-wider text-muted-foreground">
          <span className="h-px flex-1 bg-border" />
          or
          <span className="h-px flex-1 bg-border" />
        </div>
        <div className="flex items-center justify-center gap-2 rounded-lg border border-border bg-background py-2 text-xs font-medium text-foreground">
          <Image src="/GoogleLogo.png" alt="" width={14} height={14} className="size-3.5" />
          Continue with Google
        </div>
      </div>
      <span className="absolute -bottom-3 -right-3 rounded-full border border-border bg-card px-3 py-1.5 text-xs font-medium text-foreground shadow-lg">
        Or join as a guest →
      </span>
    </div>
  );
}

/** Step 2: a meeting code with copy feedback, and a CF Meet ID with its QR code. */
function ShareScene() {
  return (
    <div className="relative w-full max-w-xs space-y-3">
      <div className="rounded-2xl border border-border bg-card p-3 shadow-xl shadow-primary/10">
        <p className="px-1 text-[11px] font-medium text-muted-foreground">Meeting code</p>
        <div className="mt-1.5 flex items-center gap-2 rounded-lg bg-muted px-3 py-2">
          <Link2 className="size-3.5 shrink-0 text-primary" />
          <span className="flex-1 truncate font-mono text-xs text-foreground">kxr-mpqa-tdv</span>
          <span className="flex items-center gap-1 rounded-md bg-primary px-2 py-1 text-[11px] font-semibold text-primary-foreground">
            <Copy className="size-3" /> Copy
          </span>
        </div>
      </div>
      <div className="ml-6 flex items-center gap-3 rounded-2xl border border-border bg-card p-3 shadow-xl shadow-primary/10">
        <div className="rounded-md bg-white p-1">
          <QrCode value="CF Meet" size={44} label="Example CF Meet QR code" />
        </div>
        <div className="min-w-0 leading-tight">
          <p className="text-[11px] text-muted-foreground">Your CF Meet ID</p>
          <p className="truncate font-mono text-xs font-semibold text-foreground">CF-K7Q2M9XD4R…</p>
        </div>
      </div>
      <span className="absolute -top-3 right-2 flex items-center gap-1 rounded-full bg-[var(--success)] px-2.5 py-1 text-[11px] font-semibold text-white shadow-lg">
        <Check className="size-3" /> Copied
      </span>
    </div>
  );
}

/** Step 3: a small live call with real faces and call controls. */
function MeetScene() {
  const faces = ["/landing/meet-1.png", "/landing/meet-2.png", "/landing/meet-3.png", "/landing/hero-4.jpg"];
  return (
    <div className="relative w-full max-w-xs">
      <div className="rounded-2xl bg-slate-900 p-2 shadow-xl shadow-primary/20 ring-1 ring-slate-700/60">
        <div className="grid grid-cols-2 gap-1.5">
          {faces.map((src, i) => (
            <div
              key={src}
              className={`relative aspect-video overflow-hidden rounded-lg ${i === 0 ? "ring-2 ring-primary" : ""}`}
            >
              <Image src={src} alt="" fill sizes="140px" className="object-cover" />
            </div>
          ))}
        </div>
        <div className="mt-2 flex items-center justify-center gap-1.5">
          {[Mic, Video].map((Icon, i) => (
            <span key={i} className="flex size-7 items-center justify-center rounded-full bg-white/10 text-white">
              <Icon className="size-3.5" />
            </span>
          ))}
          <span className="flex h-7 items-center justify-center rounded-full bg-destructive px-3 text-white">
            <PhoneOff className="size-3.5" />
          </span>
        </div>
      </div>
      <span className="absolute -left-3 -top-3 flex items-center gap-1.5 rounded-full border border-border bg-card px-3 py-1.5 text-xs font-medium text-foreground shadow-lg">
        <span className="cf-pulse size-2 rounded-full bg-[var(--success)]" />
        Connected
      </span>
    </div>
  );
}

const steps = [
  {
    scene: <SignUpScene />,
    title: "Create your free account",
    body: "Sign up in a minute with email or Google. Joining a meeting as a guest needs no account at all.",
  },
  {
    scene: <ShareScene />,
    title: "Share a link or your CF Meet ID",
    body: "Send a meeting code to anyone, or share your CF Meet ID and QR code so people can connect with you.",
  },
  {
    scene: <MeetScene />,
    title: "Meet, call and message",
    body: "Talk face to face, call or chat from your computer or your phone, all in one place.",
  },
];

export default function LandingHowItWorks() {
  return (
    <section aria-labelledby="how-heading" className="relative isolate overflow-hidden border-t border-border bg-muted/30 py-14 sm:py-20 dark:bg-[#18181A]">
      
      <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
        <div className="grid gap-6 lg:grid-cols-2 lg:items-end">
          <div>
            <p className="text-sm font-semibold uppercase tracking-[0.18em] text-primary">How it works</p>
            <h2 id="how-heading" className="mt-3 text-3xl font-bold tracking-tight text-foreground sm:text-5xl">
              Up and running in minutes.
            </h2>
          </div>
          <p className="max-w-md text-base leading-relaxed text-muted-foreground sm:text-lg lg:justify-self-end">
            No downloads, no setup calls. Create an account, share a link and start talking.
          </p>
        </div>

        <ol className="mt-12 grid gap-6 md:grid-cols-3 lg:gap-8">
          {steps.map(({ scene, title, body }, i) => (
            <Reveal as="li" key={title} delay={i * 120} className="flex">
            <div
              className="group flex w-full flex-col overflow-hidden rounded-[28px] border border-border bg-card shadow-[0_12px_40px_-20px_rgba(15,23,42,0.25)] transition duration-300 hover:-translate-y-1 hover:shadow-[0_24px_60px_-24px_rgba(0,107,255,0.35)]"
            >
              <div
                aria-hidden
                className="relative flex aspect-[5/4] items-center justify-center overflow-hidden bg-muted/50 px-6"
              >
                <div className="absolute inset-0 bg-[radial-gradient(circle,rgba(0,107,255,0.14)_1px,transparent_1px)] bg-size-[18px_18px] [mask-image:radial-gradient(ellipse_at_center,black_40%,transparent_75%)]" />
                <div className="relative w-full transition-transform duration-300 group-hover:scale-[1.03]">
                  <div className="flex justify-center">{scene}</div>
                </div>
              </div>

              <div className="flex flex-1 flex-col p-7">
                <span className="text-4xl font-bold tracking-tight text-primary">
                  {String(i + 1).padStart(2, "0")}
                </span>
                <h3 className="mt-3 text-xl font-semibold text-foreground">{title}</h3>
                <p className="mt-2 text-base leading-relaxed text-muted-foreground">{body}</p>
              </div>
            </div>
            </Reveal>
          ))}
        </ol>
      </div>
    </section>
  );
}
