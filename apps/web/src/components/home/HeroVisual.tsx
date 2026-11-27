import Image from "next/image";
import {
  FileText,
  Hand,
  Mic,
  MicOff,
  MessageSquare,
  MonitorUp,
  MoreHorizontal,
  Phone,
  PhoneOff,
  SendHorizontal,
  ShieldCheck,
  Smile,
  Users,
  Video,
} from "lucide-react";
import { cn } from "@/lib/utils";

const participants = [
  { name: "Tunde", src: "/landing/hero-1.jpg" },
  { name: "Amara", src: "/landing/hero-2.jpg", speaking: true },
  { name: "Kwame", src: "/landing/hero-3.jpg", muted: true },
  { name: "Mei", src: "/landing/hero-4.jpg" },
  { name: "Arjun", src: "/landing/hero-5.jpg", muted: true },
  { name: "Sofia", src: "/landing/hero-6.jpg" },
];

const toolbar = [
  { icon: Mic, label: "Mute" },
  { icon: Video, label: "Camera" },
  { icon: MonitorUp, label: "Share" },
  { icon: Users, label: "People" },
  { icon: MessageSquare, label: "Chat" },
  { icon: Smile, label: "React" },
  { icon: MoreHorizontal, label: "More" },
];

/** A front-facing laptop (aluminium lid, black bezel, camera, hinged base) showing a meeting. */
function Laptop() {
  return (
    <div className="relative">
      {/* Lid */}
      <div className="rounded-[2.4cqw] bg-linear-to-b from-slate-300 to-slate-400 p-[0.45cqw] shadow-[0_30px_60px_-20px_rgba(15,23,42,0.45)] dark:from-slate-500 dark:to-slate-600">
        <div className="relative rounded-[2cqw] bg-black px-[1.6cqw] pb-[1.6cqw] pt-[2.4cqw]">
          <span className="absolute left-1/2 top-[0.8cqw] size-[0.8cqw] -translate-x-1/2 rounded-full bg-slate-700 ring-[0.2cqw] ring-slate-800" />
          {/* Screen */}
          <div className="overflow-hidden rounded-[0.6cqw] bg-[#0B1220]">
            <div className="flex items-center justify-between px-[1.8cqw] py-[1.3cqw] text-[clamp(6px,1.5cqw,11px)] text-slate-300">
              <div className="flex items-center gap-[1.2cqw]">
                <span className="font-semibold text-white">Weekly team sync</span>
                <span className="flex items-center gap-[0.6cqw]">
                  <span className="size-[0.9cqw] rounded-full bg-[var(--success)]" />6 people
                </span>
              </div>
              <div className="flex items-center gap-[1.2cqw]">
                <span className="flex items-center gap-[0.6cqw] rounded-full bg-white/10 px-[1.2cqw] py-[0.4cqw]">
                  <ShieldCheck className="size-[1.6cqw]" />
                  Waiting room on
                </span>
                <span className="tabular-nums">24:16</span>
              </div>
            </div>

            <div className="grid grid-cols-3 gap-[0.9cqw] px-[1.3cqw]">
              {participants.map((p) => (
                <div
                  key={p.name}
                  className={cn(
                    "relative aspect-video overflow-hidden rounded-[0.9cqw] bg-slate-800",
                    p.speaking && "cf-speak ring-[0.35cqw] ring-primary",
                  )}
                >
                  <Image src={p.src} alt="" fill sizes="(min-width: 1024px) 200px, 30vw" className="object-cover" priority />
                  <span className="absolute bottom-[0.7cqw] left-[0.7cqw] flex items-center gap-[0.5cqw] rounded-full bg-black/55 px-[0.9cqw] py-[0.25cqw] text-[clamp(5px,1.3cqw,10px)] font-medium text-white backdrop-blur-sm">
                    {p.muted ? <MicOff className="size-[1.3cqw] text-red-400" /> : <Mic className="size-[1.3cqw]" />}
                    {p.name}
                  </span>
                </div>
              ))}
            </div>

            <div className="flex items-center justify-center gap-[1.2cqw] px-[1.8cqw] py-[1.6cqw]">
              {toolbar.map(({ icon: Icon, label }) => (
                <span key={label} className="flex size-[4.2cqw] items-center justify-center rounded-full bg-white/10 text-white">
                  <Icon className="size-[1.9cqw]" />
                </span>
              ))}
              <span className="ml-[0.6cqw] flex h-[4.2cqw] items-center justify-center rounded-full bg-destructive px-[2.2cqw] text-white">
                <PhoneOff className="size-[1.9cqw]" />
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Base */}
      <div className="relative -mx-[7%] h-[2.2cqw] rounded-b-[2.6cqw] rounded-t-[0.4cqw] bg-linear-to-b from-slate-200 via-slate-300 to-slate-400 shadow-[0_18px_30px_-12px_rgba(15,23,42,0.5)] dark:from-slate-500 dark:via-slate-600 dark:to-slate-700">
        <span className="absolute left-1/2 top-0 h-[0.9cqw] w-[14%] -translate-x-1/2 rounded-b-[0.9cqw] bg-slate-400/70 dark:bg-slate-800/60" />
      </div>
    </div>
  );
}

/** A modern phone showing a team chat. */
function PhoneChat() {
  return (
    <div className="rounded-[4.6cqw] bg-slate-900 p-[0.7cqw] shadow-[0_30px_60px_-20px_rgba(15,23,42,0.55)] ring-1 ring-slate-700">
      <div className="relative flex aspect-[9/19] flex-col overflow-hidden rounded-[4cqw] bg-card text-[clamp(5px,1.35cqw,10px)]">
        <span className="absolute left-1/2 top-[1cqw] h-[1.6cqw] w-[32%] -translate-x-1/2 rounded-full bg-black" />
        <div className="flex items-center gap-[1cqw] border-b border-border px-[1.8cqw] pb-[1.2cqw] pt-[4.2cqw]">
          <span className="flex size-[4cqw] items-center justify-center rounded-full bg-primary/15 font-bold text-primary">DT</span>
          <div className="leading-tight">
            <p className="font-semibold text-foreground">Design Team</p>
            <p className="text-muted-foreground">4 members</p>
          </div>
        </div>

        <div className="flex flex-1 flex-col gap-[1.2cqw] bg-muted/50 px-[1.4cqw] py-[1.8cqw]">
          <div className="flex items-end gap-[0.7cqw]">
            <Image src="/landing/avatar-chat.jpg" alt="" width={48} height={48} className="size-[3cqw] rounded-full object-cover" />
            <p className="rounded-[1.4cqw] rounded-bl-[0.3cqw] bg-card px-[1.2cqw] py-[0.9cqw] text-foreground shadow-sm">
              Great meeting today!
            </p>
          </div>
          <p className="ml-auto max-w-[85%] rounded-[1.4cqw] rounded-br-[0.3cqw] bg-primary px-[1.2cqw] py-[0.9cqw] text-primary-foreground">
            Thanks! Sharing the plan now.
          </p>
          <div className="ml-auto flex w-[85%] items-center gap-[0.9cqw] rounded-[1.2cqw] border border-border bg-card px-[1cqw] py-[0.9cqw]">
            <FileText className="size-[2.4cqw] shrink-0 text-primary" />
            <div className="min-w-0 leading-tight">
              <p className="truncate font-semibold text-foreground">Project-plan.pdf</p>
              <p className="text-muted-foreground">2.4 MB</p>
            </div>
          </div>
          <div className="flex items-end gap-[0.7cqw]">
            <Image src="/landing/avatar-chat.jpg" alt="" width={48} height={48} className="size-[3cqw] rounded-full object-cover" />
            <p className="rounded-[1.4cqw] rounded-bl-[0.3cqw] bg-card px-[1.2cqw] py-[0.9cqw] text-foreground shadow-sm">Looks good 👍</p>
          </div>
        </div>

        <div className="flex items-center gap-[0.9cqw] border-t border-border px-[1.4cqw] pb-[2.4cqw] pt-[1.2cqw]">
          <span className="flex-1 rounded-full bg-muted px-[1.2cqw] py-[0.7cqw] text-muted-foreground">Message</span>
          <SendHorizontal className="size-[2.2cqw] text-primary" />
        </div>
      </div>
    </div>
  );
}

/**
 * Decorative product scene for the landing hero: a meeting on a laptop, a team chat on a
 * phone beside it and an incoming-call card. Built in code (not a screenshot) so it stays
 * sharp and follows the light/dark theme. Container units keep every part in proportion.
 */
export default function HeroVisual({ className }: { className?: string }) {
  return (
    <div
      role="img"
      aria-label="CF Meet on a laptop and a phone: a video meeting with six people, a team chat and an incoming call."
      className={cn("@container mx-auto w-full max-w-[720px] select-none", className)}
    >
      <div aria-hidden className="relative pb-[4cqw] pt-[3cqw]">
        {/* Brand glow behind the devices */}
        <div className="absolute inset-x-[8%] inset-y-[10%] -z-10 rounded-full bg-linear-to-tr from-primary/25 via-accent/20 to-transparent blur-[6cqw] dark:from-primary/35 dark:via-accent/15" />

        <div className="ml-[4%] w-[76%]">
          <Laptop />
        </div>

        <div className="absolute bottom-[1cqw] right-0 w-[22%]">
          <PhoneChat />
        </div>

        {/* Raised-hand toast above the laptop corner */}
        <div className="cf-float absolute left-0 top-0 flex items-center gap-[1cqw] rounded-full border border-border bg-card/95 py-[0.9cqw] pl-[0.9cqw] pr-[1.8cqw] text-[clamp(6px,1.5cqw,11px)] shadow-lg backdrop-blur">
          <span className="flex size-[3.6cqw] items-center justify-center rounded-full bg-amber-100 text-amber-600 dark:bg-amber-500/20 dark:text-amber-400">
            <Hand className="size-[1.9cqw]" />
          </span>
          <span className="font-medium text-foreground">Mei raised a hand</span>
        </div>

        {/* Incoming call card, over the lower edge of the phone */}
        <div style={{ animationDelay: "-3s" }}
          className="cf-float absolute bottom-0 right-[2%] flex w-[33%] items-center gap-[1.2cqw] rounded-[2cqw] border border-border bg-card/95 p-[1.3cqw] shadow-xl backdrop-blur">
          <Image src="/landing/avatar-caller.jpg" alt="" width={64} height={64} className="size-[4.8cqw] rounded-full object-cover" />
          <div className="min-w-0 flex-1 leading-tight text-[clamp(6px,1.5cqw,11px)]">
            <p className="text-muted-foreground">Incoming call</p>
            <p className="truncate font-semibold text-foreground">Ada Nwosu</p>
          </div>
          <span className="flex size-[4cqw] items-center justify-center rounded-full bg-destructive text-white">
            <PhoneOff className="size-[1.8cqw]" />
          </span>
          <span className="flex size-[4cqw] items-center justify-center rounded-full bg-[var(--success)] text-white">
            <Phone className="size-[1.8cqw]" />
          </span>
        </div>
      </div>
    </div>
  );
}
