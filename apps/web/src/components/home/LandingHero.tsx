import Image from "next/image";
import Link from "next/link";
import { Captions, FileText, Hand, Mic, MonitorUp, PhoneOff, SendHorizontal, Smile, Video } from "lucide-react";
import { Button } from "@/components/ui/button";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";

const TILES = [
  { src: "/landing/meet-1.png", name: "Alicia" },
  { src: "/landing/meet-2.png", name: "Jordan" },
  { src: "/landing/meet-3.png", name: "Ravi" },
];

const CHAT = [
  { who: "Alicia", time: "10:02", text: "Here's the latest draft of the plan. Can we review it together?", avatar: "/landing/meet-1.png" },
  { who: "Jordan", time: "10:05", text: "Looks good. I added a few notes on the launch tasks.", avatar: "/landing/meet-2.png" },
  { who: "Ravi", time: "10:07", text: "Great, I'll finish the checklist and share it here.", avatar: "/landing/meet-3.png", file: "Launch checklist.pdf" },
];

function Tile({ src, name, className, priority }: { src: string; name: string; className?: string; priority?: boolean }) {
  return (
    <div className={`relative min-h-0 overflow-hidden rounded-xl ${className ?? ""}`}>
      <Image src={src} alt="" fill sizes="(min-width: 1024px) 40vw, 60vw" className="object-cover" priority={priority} />
      <span className="absolute bottom-2 left-2 rounded-md bg-black/50 px-2 py-0.5 text-[11px] font-medium text-white">{name}</span>
    </div>
  );
}

const ctl = "flex size-9 items-center justify-center rounded-full bg-[#3C4043] text-white [&_svg]:size-4 sm:size-10";

/** Example meeting window, styled like the real CF Meet room (dark stage, round controls, chat panel). */
function MeetingPreview() {
  return (
    <div aria-hidden className="overflow-hidden rounded-2xl bg-[#202124] text-left text-white shadow-[0_30px_70px_-30px_rgba(15,23,42,0.55)] ring-1 ring-black/5">
      <div className="grid gap-3 p-3 sm:p-4 lg:grid-cols-[minmax(0,1fr)_17rem]">
        <div className="min-w-0">
          <div className="grid grid-cols-3 gap-2 sm:gap-3">
            <Tile {...TILES[0]} className="col-span-2 aspect-[4/3]" priority />
            <div className="grid grid-rows-2 gap-2 sm:gap-3">
              <Tile {...TILES[1]} />
              <Tile {...TILES[2]} />
            </div>
          </div>
          <div className="mt-3 flex items-center gap-2 sm:mt-4">
            <span className="hidden text-sm font-medium text-white/90 sm:block">10:12 · Weekly plan</span>
            <div className="mx-auto flex items-center gap-2 sm:mx-0 sm:ml-auto sm:mr-auto">
              <span className={ctl}><Mic /></span>
              <span className={ctl}><Video /></span>
              <span className={ctl}><Captions /></span>
              <span className={`${ctl} hidden sm:flex`}><Smile /></span>
              <span className={`${ctl} hidden sm:flex`}><Hand /></span>
              <span className={ctl}><MonitorUp /></span>
              <span className="flex h-9 w-14 items-center justify-center rounded-full bg-[#EA4335] [&_svg]:size-4 sm:h-10"><PhoneOff /></span>
            </div>
            <span className="hidden w-[7.5rem] sm:block" />
          </div>
        </div>
        <div className="hidden flex-col rounded-xl bg-[#2D2E31] p-4 lg:flex">
          <p className="text-sm font-semibold">In-call messages</p>
          <ul className="mt-4 flex-1 space-y-4">
            {CHAT.map((m) => (
              <li key={m.who} className="flex gap-2.5">
                <Image src={m.avatar} alt="" width={28} height={28} className="size-7 shrink-0 rounded-full object-cover" />
                <div className="min-w-0">
                  <p className="text-xs"><span className="font-semibold">{m.who}</span> <span className="text-white/50">{m.time}</span></p>
                  <p className="mt-0.5 text-xs leading-relaxed text-white/75">{m.text}</p>
                  {m.file && <p className="mt-2 inline-flex items-center gap-2 rounded-lg bg-white/8 px-2.5 py-1.5 text-xs"><FileText className="size-3.5 text-[#8AB4FF]" />{m.file}</p>}
                </div>
              </li>
            ))}
          </ul>
          <div className="mt-3 flex h-9 items-center gap-2 rounded-full bg-[#3C4043] px-3.5 text-xs text-white/60">
            <span className="flex-1">Send a message</span><SendHorizontal className="size-3.5 text-[#8AB4FF]" />
          </div>
        </div>
      </div>
    </div>
  );
}

export default function LandingHero() {
  return (
    <section aria-labelledby="hero-heading" className="relative isolate">
      <div className="mx-auto max-w-6xl px-4 pb-4 pt-12 text-center sm:px-6 sm:pt-20 lg:px-8 lg:pb-6">
          <p className="text-xs font-semibold uppercase tracking-[0.14em] text-muted-foreground">Meetings · Calls · Chat</p>
          <h1 id="hero-heading" className="mx-auto mt-4 max-w-3xl text-[2.6rem] font-bold leading-[1.04] tracking-tight text-foreground sm:text-6xl lg:text-[4.25rem]">
            Less distance.<br />More getting together.
          </h1>
          <p className="mx-auto mt-5 max-w-xl text-base leading-relaxed text-muted-foreground sm:text-lg">
            One clear place to meet, call and keep the conversation going, with your team and everyone you work with.
          </p>
          <div className="mx-auto mt-8 flex max-w-2xl flex-col items-stretch gap-3 sm:flex-row sm:items-center">
            <Button asChild className="h-12 shrink-0 rounded-xl px-6 text-base shadow-md shadow-primary/25">
              <Link href="/dashboard"><Video className="size-5" />New meeting</Link>
            </Button>
            <JoinMeetingInput
              className="flex-1"
              inputId="hero-join"
              placeholder="Enter a code or link"
              inputClassName="h-12 rounded-xl px-4 text-base md:h-12 md:text-base"
              buttonClassName="h-12 rounded-xl px-6 shadow-none disabled:bg-transparent disabled:text-muted-foreground disabled:opacity-100"
            />
          </div>
          <div className="mt-12 sm:mt-14"><MeetingPreview /></div>
      </div>
    </section>
  );
}
