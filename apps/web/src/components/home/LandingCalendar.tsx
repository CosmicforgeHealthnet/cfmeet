import Image from "next/image";
import Link from "next/link";
import { CalendarPlus, Clock, Globe, Repeat, Video } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import Reveal from "./Reveal";

const days = [
  { name: "Mon", date: 10 },
  { name: "Tue", date: 11 },
  { name: "Wed", date: 12, today: true },
  { name: "Thu", date: 13 },
  { name: "Fri", date: 14 },
];
const hours = ["9 AM", "10 AM", "11 AM", "12 PM", "1 PM"];
const SLOT = 56; // px per hour

type Ev = { day: number; start: number; length: number; title: string; time: string; tone: string; people?: string[]; recurring?: boolean };
const events: Ev[] = [
  { day: 0, start: 0, length: 1, title: "Team sync", time: "9:00 – 10:00", tone: "blue", recurring: true, people: ["/landing/hero-1.jpg", "/landing/hero-2.jpg"] },
  { day: 1, start: 1.5, length: 1.5, title: "Project review", time: "10:30 – 12:00", tone: "violet", people: ["/landing/hero-3.jpg", "/landing/hero-4.jpg", "/landing/hero-5.jpg"] },
  { day: 2, start: 0, length: 1, title: "Team sync", time: "9:00 – 10:00", tone: "blue", recurring: true },
  { day: 3, start: 0.5, length: 1, title: "Design critique", time: "9:30 – 10:30", tone: "amber" },
  { day: 3, start: 3, length: 1, title: "1:1 with Ada", time: "12:00 – 1:00", tone: "green" },
  { day: 4, start: 2, length: 1, title: "Client call", time: "11:00 – 12:00", tone: "blue", people: ["/landing/hero-6.jpg"] },
];

const tones: Record<string, React.CSSProperties> = {
  blue: { background: "linear-gradient(rgba(0,107,255,0.08), rgba(0,107,255,0.08)), var(--card)", borderColor: "rgba(0,107,255,0.28)", color: "#0057D9" },
  violet: { background: "linear-gradient(rgba(124,58,237,0.08), rgba(124,58,237,0.08)), var(--card)", borderColor: "rgba(124,58,237,0.28)", color: "#6D28D9" },
  amber: { background: "linear-gradient(rgba(245,158,11,0.10), rgba(245,158,11,0.10)), var(--card)", borderColor: "rgba(245,158,11,0.35)", color: "#B45309" },
  green: { background: "linear-gradient(rgba(16,185,129,0.09), rgba(16,185,129,0.09)), var(--card)", borderColor: "rgba(16,185,129,0.30)", color: "#047857" },
};

function WeekCalendar() {
  return (
    <div aria-hidden className="relative">

      <div className="overflow-hidden rounded-[28px] border border-border bg-card shadow-sm">
        {/* Toolbar */}
        <div className="flex items-center justify-between gap-3 px-5 pb-3 pt-5">
          <div>
            <p className="text-lg font-bold text-foreground">November 2026</p>
            <p className="flex items-center gap-1 text-xs text-muted-foreground">
              <Globe className="size-3" /> Africa/Lagos · WAT
            </p>
          </div>
          <div className="flex rounded-full bg-muted p-1 text-xs font-medium">
            <span className="rounded-full px-3 py-1 text-muted-foreground">Day</span>
            <span className="rounded-full bg-card px-3 py-1 text-foreground shadow-sm">Week</span>
            <span className="rounded-full px-3 py-1 text-muted-foreground">Month</span>
          </div>
        </div>

        {/* Day headers */}
        <div className="flex px-3">
          <div className="w-12 shrink-0" />
          {days.map((d) => (
            <div key={d.date} className={cn("flex-1 flex-col items-center pb-3", d.date > 12 ? "hidden sm:flex" : "flex")}>
              <span className="text-[11px] font-medium uppercase text-muted-foreground">{d.name}</span>
              <span
                className={cn(
                  "mt-1 flex size-8 items-center justify-center rounded-full text-sm font-semibold",
                  d.today ? "bg-primary text-primary-foreground" : "text-foreground",
                )}
              >
                {d.date}
              </span>
            </div>
          ))}
        </div>

        {/* Grid */}
        <div className="flex border-t border-border px-3 pb-4 pt-3">
          <div className="w-12 shrink-0">
            {hours.map((h) => (
              <div key={h} style={{ height: SLOT }} className="relative">
                <span className="absolute right-2 top-0 -translate-y-1/2 bg-card px-0.5 text-[10px] leading-none text-muted-foreground">
                  {h}
                </span>
              </div>
            ))}
          </div>

          {days.map((d, di) => (
            <div key={d.date} className={cn("relative flex-1 border-l border-border/70", d.date > 12 && "hidden sm:block", d.today && "bg-primary/[0.03]")}>
              {hours.map((h, hi) => (
                <div key={h} style={{ height: SLOT }} className={hi === 0 ? "" : "border-t border-border/60"} />
              ))}


              {events
                .filter((e) => e.day === di)
                .map((e) => (
                  <div
                    key={e.title + e.start}
                    className={cn(
                      "absolute inset-x-1 z-10 overflow-hidden rounded-[10px] border px-2 py-1.5 text-[11px] leading-tight",
                    )}
                    style={{ ...tones[e.tone], top: e.start * SLOT + 3, height: e.length * SLOT - 6 }}
                  >
                    <p className="flex items-center gap-1 truncate font-semibold">
                      {e.title}
                      {e.recurring && <Repeat className="size-2.5 shrink-0" />}
                    </p>
                    <p className="truncate opacity-70">{e.time}</p>
                    {e.people && e.length > 1 && (
                      <div className="mt-1.5 flex -space-x-1.5">
                        {e.people.map((p) => (
                          <Image key={p} src={p} alt="" width={40} height={40} className="size-5 rounded-full object-cover ring-2 ring-card" />
                        ))}
                      </div>
                    )}
                  </div>
                ))}
            </div>
          ))}
        </div>
      </div>

      {/* Floating meeting card */}
      <div className="cf-float absolute -bottom-8 -left-4 w-64 rounded-2xl border border-border bg-card p-4 shadow-lg sm:-left-8">
        <div className="flex items-center gap-2">
          <span className="flex size-8 items-center justify-center rounded-lg bg-primary/10 text-primary">
            <Video className="size-4" />
          </span>
          <div className="min-w-0">
            <p className="truncate text-sm font-semibold text-foreground">Team sync</p>
            <p className="flex items-center gap-1 text-xs text-muted-foreground">
              <Clock className="size-3" /> Starts in 5 min
            </p>
          </div>
        </div>
        <div className="mt-3 flex items-center justify-between">
          <div className="flex -space-x-2">
            {["/landing/hero-1.jpg", "/landing/hero-2.jpg", "/landing/hero-4.jpg"].map((p) => (
              <Image key={p} src={p} alt="" width={56} height={56} className="size-7 rounded-full object-cover ring-2 ring-card" />
            ))}
            <span className="flex size-7 items-center justify-center rounded-full bg-muted text-[10px] font-semibold text-muted-foreground ring-2 ring-card">
              +3
            </span>
          </div>
          <span className="rounded-full bg-primary px-4 py-1.5 text-xs font-semibold text-primary-foreground">Join</span>
        </div>
      </div>
    </div>
  );
}

export default function LandingCalendar() {
  return (
    <section aria-labelledby="calendar-heading" className="py-14 sm:py-20">
      <div className="mx-auto grid max-w-7xl items-center gap-16 px-4 sm:px-6 lg:grid-cols-[minmax(0,0.85fr)_minmax(0,1.15fr)] lg:px-8">
        <Reveal className="min-w-0 max-w-lg">
          <p className="text-sm font-semibold uppercase tracking-[0.18em] text-primary">Calendar</p>
          <h2 id="calendar-heading" className="mt-3 text-3xl font-bold tracking-tight text-foreground sm:text-5xl">
            Plan your week without the back-and-forth.
          </h2>
          <p className="mt-5 text-base leading-relaxed text-muted-foreground sm:text-lg">
            Schedule meetings, set up repeating series and keep everyone on the same page, with the right time
            zone for every guest.
          </p>
          <Button asChild className="mt-8 h-12 rounded-xl px-7 shadow-md shadow-primary/25">
            <Link href="/signup">
              <CalendarPlus className="size-4" /> Schedule a meeting
            </Link>
          </Button>
        </Reveal>

        <Reveal delay={120} className="min-w-0 pb-8">
          <WeekCalendar />
        </Reveal>
      </div>
    </section>
  );
}
