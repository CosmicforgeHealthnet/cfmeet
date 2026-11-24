import Image from "next/image";
import Link from "next/link";
import { MicOff } from "lucide-react";

const tiles: { name: string; src: string; speaking?: boolean; muted?: boolean; position?: string }[] = [
  { name: "Alicia", src: "/landing/meet-1.png" },
  { name: "Grace", src: "/landing/auth-grace.jpg", speaking: true },
  { name: "Jordan", src: "/landing/meet-2.png", muted: true },
  { name: "Mei", src: "/landing/hero-4.jpg" },
  { name: "Ravi", src: "/landing/meet-3.png", muted: true },
  { name: "Sofia", src: "/landing/hero-6.jpg" },
];

/**
 * Account screens. Phones: the form, full screen. Large screens: the form beside a panel
 * with real people on a call; the page fits the window so nothing hides below the fold.
 */
export default function AuthLayout({ children }: { children: React.ReactNode }) {
  return (
    <div className="min-h-screen bg-background lg:grid lg:h-screen lg:grid-cols-2 lg:overflow-hidden">
      <div className="no-scrollbar flex min-h-screen flex-col px-5 py-6 sm:px-10 lg:min-h-0 lg:overflow-y-auto lg:px-16">
        <Link href="/" aria-label="CF Meet home" className="inline-flex w-fit">
          <Image src="/light-mode-cf-meet.png" alt="CF Meet" width={120} height={40} className="h-8 w-auto dark:hidden" priority />
          <Image src="/dark-mode-cfmeet.png" alt="CF Meet" width={120} height={40} className="hidden h-8 w-auto dark:block" priority />
        </Link>
        <main className="flex flex-1 items-center justify-center py-6">
          <div className="w-full max-w-[400px]">{children}</div>
        </main>
      </div>

      <aside aria-hidden className="hidden h-full p-3 lg:block">
        {/* Like a meeting in Google Meet: separate rounded tiles on a dark stage, names bottom-left. */}
        <div className="flex h-full flex-col rounded-[24px] bg-[#202124] p-3">
          <div className="grid min-h-0 flex-1 grid-cols-2 grid-rows-3 gap-2">
            {tiles.map((t) => (
              <div key={t.name} className="relative overflow-hidden rounded-xl bg-[#3C4043]">
                <Image src={t.src} alt="" fill sizes="25vw" className="object-cover" style={{ objectPosition: t.position ?? "center" }} priority />
                {t.speaking ? <span className="pointer-events-none absolute inset-0 rounded-xl ring-[3px] ring-inset ring-[#8AB4F8]" /> : null}
                <span className="absolute bottom-2.5 left-2.5 flex items-center gap-1.5 rounded-md bg-black/50 px-2 py-1 text-xs font-medium text-white backdrop-blur-sm">
                  {t.name}
                  {t.speaking ? (
                    <span className="flex size-4 items-center justify-center gap-[2px] rounded-full bg-[#1A73E8]">
                      {[0, 1, 2].map((b) => (
                        <span key={b} className="cf-eq w-[2px] rounded-full bg-white" style={{ animationDelay: `${b * 0.15}s` }} />
                      ))}
                    </span>
                  ) : t.muted ? (
                    <MicOff className="size-3 text-red-300" />
                  ) : null}
                </span>
              </div>
            ))}
          </div>
          <div className="px-3 pb-3 pt-6 xl:px-5 xl:pb-5">
            <p className="max-w-md text-2xl font-bold leading-tight tracking-tight text-white xl:text-3xl">
              Meet, call and message, all in one place.
            </p>
            <p className="mt-2 max-w-md text-sm text-white/70 xl:text-base">
              Video meetings, calls, chats and your contacts, together in CF Meet.
            </p>
          </div>
        </div>
      </aside>
    </div>
  );
}
