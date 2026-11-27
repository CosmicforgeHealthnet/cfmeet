import Image from "next/image";
import type { LucideIcon } from "lucide-react";
import { Contact, Heart, MessageSquare, Phone, PhoneOff, ThumbsUp, Video } from "lucide-react";
import QrCode from "@/components/communications/QrCode";
import { cn } from "@/lib/utils";
import Reveal from "./Reveal";

/** Small product cards laid over each photo, so the product is visible in every story. */
function WaitingRoomCard() {
  return (
    <div className="w-56 rounded-2xl border border-border bg-card/95 p-4 shadow-xl backdrop-blur">
      <p className="text-sm font-semibold text-foreground">Waiting room (2)</p>
      <div className="mt-3 flex items-center justify-between gap-3">
        <div className="flex -space-x-2">
          {["/landing/avatar-chat.jpg", "/landing/avatar-caller.jpg"].map((src) => (
            <Image key={src} src={src} alt="" width={64} height={64} className="size-8 rounded-full object-cover ring-2 ring-card" />
          ))}
        </div>
        <span className="rounded-full bg-primary px-4 py-1.5 text-xs font-semibold text-primary-foreground">Admit</span>
      </div>
    </div>
  );
}

function IncomingCallCard() {
  return (
    <div className="flex w-64 items-center gap-3 rounded-2xl border border-border bg-card/95 p-3 shadow-xl backdrop-blur">
      <Image src="/landing/avatar-caller.jpg" alt="" width={80} height={80} className="size-10 rounded-full object-cover" />
      <div className="min-w-0 flex-1 leading-tight">
        <p className="text-xs text-muted-foreground">Incoming call</p>
        <p className="truncate text-sm font-semibold text-foreground">Ada Nwosu</p>
      </div>
      <span className="flex size-9 items-center justify-center rounded-full bg-destructive text-white">
        <PhoneOff className="size-4" />
      </span>
      <span className="flex size-9 items-center justify-center rounded-full bg-[var(--success)] text-white">
        <Phone className="size-4" />
      </span>
    </div>
  );
}

function ChatCard() {
  return (
    <div className="w-60 rounded-2xl border border-border bg-card/95 p-4 shadow-xl backdrop-blur">
      <div className="flex items-center gap-2">
        <span className="flex size-8 items-center justify-center rounded-full bg-primary/15 text-xs font-bold text-primary">DT</span>
        <p className="text-sm font-semibold text-foreground">Design Team</p>
      </div>
      <p className="mt-3 rounded-xl rounded-tl-sm bg-muted px-3 py-2 text-sm text-foreground">Let&apos;s ship this today 🚀</p>
      <div className="mt-2 flex gap-1.5 text-xs text-muted-foreground">
        <span className="flex items-center gap-1 rounded-full border border-border bg-card px-2 py-0.5">
          <ThumbsUp className="size-3 text-primary" /> 3
        </span>
        <span className="flex items-center gap-1 rounded-full border border-border bg-card px-2 py-0.5">
          <Heart className="size-3 text-destructive" /> 2
        </span>
      </div>
    </div>
  );
}

function MeetIdCard() {
  return (
    <div className="flex w-64 items-center gap-4 rounded-2xl border border-border bg-card/95 p-4 shadow-xl backdrop-blur">
      <div className="min-w-0 flex-1">
        <p className="text-xs text-muted-foreground">Your CF Meet ID</p>
        <p className="mt-1 truncate font-mono text-sm font-semibold text-foreground">CF-K7Q2M9XD4R…</p>
        <p className="mt-2 text-xs text-muted-foreground">Scan to connect</p>
      </div>
      <div className="rounded-lg bg-white p-1.5">
        <QrCode value="CF Meet" size={64} label="Example CF Meet QR code" />
      </div>
    </div>
  );
}

type Feature = {
  id: string;
  icon: LucideIcon;
  title: string;
  body: string;
  image: string;
  alt: string;
  card: React.ReactNode;
};

const features: Feature[] = [
  {
    id: "meetings",
    icon: Video,
    title: "Meetings that feel like being in the same room.",
    body: "Start an instant meeting or schedule one for later. Share your screen, run polls and Q&A, and let guests in from the waiting room.",
    image: "/landing/feature-meetings.jpg",
    alt: "Three colleagues smiling together during a meeting at a table with a laptop.",
    card: <WaitingRoomCard />,
  },
  {
    id: "calls",
    icon: Phone,
    title: "Call anyone on CF Meet, voice or video.",
    body: "Clear, reliable calls between CF Meet apps, with call history and voicemail so you never lose track.",
    image: "/landing/feature-calls.jpg",
    alt: "A smiling woman on a video call on her phone outdoors.",
    card: <IncomingCallCard />,
  },
  {
    id: "messages",
    icon: MessageSquare,
    title: "Chats that keep work and life moving.",
    body: "Direct and group chats with files, reactions and replies, and search across all your conversations.",
    image: "/landing/feature-messages.jpg",
    alt: "A young man smiling while typing a message on his phone on a park bench.",
    card: <ChatCard />,
  },
  {
    id: "contacts",
    icon: Contact,
    title: "Share your CF Meet ID, no phone number needed.",
    body: "Every account gets a CF Meet ID and QR code. Scan it to connect in seconds and keep your number private.",
    image: "/landing/feature-contacts.jpg",
    alt: "Two friends smiling while looking at a phone together.",
    card: <MeetIdCard />,
  },
];

export default function LandingFeatures() {
  return (
    <section id="product" aria-labelledby="features-heading" className="scroll-mt-20 pb-14 pt-10 sm:pb-20 sm:pt-14">
      <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
        <div className="mx-auto max-w-2xl text-center">
          <h2 id="features-heading" className="text-3xl font-bold tracking-tight text-foreground sm:text-4xl">
            Everything you need to stay connected
          </h2>
          <p className="mt-4 text-base text-muted-foreground sm:text-lg">
            From meetings to messages, CF Meet has you covered.
          </p>
        </div>

        <div className="mt-12 space-y-16 sm:mt-16 lg:space-y-24">
          {features.map((f, i) => {
            const Icon = f.icon;
            const imageRight = i % 2 === 1;
            return (
              <article
                key={f.id}
                id={f.id}
                className="grid scroll-mt-20 items-center gap-14 lg:grid-cols-2 lg:gap-16"
              >
                <Reveal variant={imageRight ? "right" : "left"} className={cn("relative", imageRight && "lg:order-2")}>
                  <div className="relative aspect-[4/3] overflow-hidden rounded-3xl bg-muted">
                    <Image
                      src={f.image}
                      alt={f.alt}
                      fill
                      sizes="(min-width: 1024px) 600px, 100vw"
                      className="object-cover"
                    />
                  </div>
                  <div
                    className={cn(
                      "absolute -bottom-6 scale-90 sm:scale-100",
                      imageRight ? "left-2 origin-bottom-left sm:-left-6" : "right-2 origin-bottom-right sm:-right-6",
                    )}
                  >
                    <Reveal variant="scale" delay={350}>{f.card}</Reveal>
                  </div>
                </Reveal>

                <Reveal delay={120} className={cn("max-w-lg", imageRight && "lg:order-1")}>
                  <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary">
                    <Icon className="size-6" aria-hidden />
                  </span>
                  <h3 className="mt-6 text-2xl font-bold tracking-tight text-foreground sm:text-3xl">{f.title}</h3>
                  <p className="mt-4 text-base leading-relaxed text-muted-foreground sm:text-lg">{f.body}</p>
                </Reveal>
              </article>
            );
          })}
        </div>
      </div>
    </section>
  );
}
