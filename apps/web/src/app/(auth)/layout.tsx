import Image from "next/image";
import Link from "next/link";
import { Mic, MicOff } from "lucide-react";

const tiles: { name: string; src: string; speaking?: boolean; muted?: boolean; position?: string }[] = [
  { name: "Tunde", src: "/landing/auth-1.jpg" },
  { name: "Grace", src: "/landing/auth-grace.jpg", speaking: true },
  { name: "Kwame", src: "/landing/hero-3.jpg", muted: true },
  { name: "Mei", src: "/landing/hero-4.jpg" },
  { name: "Arjun", src: "/landing/hero-5.jpg", muted: true },
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
        <div className="relative h-full overflow-hidden rounded-[24px]">
          <div className="grid h-full grid-cols-2 grid-rows-3 gap-1.5 bg-background">
            {tiles.map((t, i) => (
              <div key={t.name} className="relative overflow-hidden bg-muted">
                <Image src={t.src} alt="" fill sizes="25vw" className="object-cover" style={{ objectPosition: t.position ?? "center" }} priority />
                <span className="absolute left-3 top-3 flex items-center gap-1.5 rounded-full bg-black/45 px-2.5 py-1 text-xs font-medium text-white backdrop-blur-sm">
                  {t.speaking ? (
                    <span className="flex size-4 items-center justify-center gap-[2px] rounded-full bg-primary">
                      {[0, 1, 2].map((b) => (
                        <span key={b} className="cf-eq w-[2px] rounded-full bg-white" style={{ animationDelay: `${b * 0.15}s` }} />
                      ))}
                    </span>
                  ) : t.muted ? (
                    <MicOff className="size-3 text-red-300" />
                  ) : (
                    <Mic className="size-3" />
                  )}
                  {t.name}
                </span>
                {t.speaking && (
                  <span
                    className="pointer-events-none absolute inset-0 ring-2 ring-inset ring-primary"
                    // Follow the panel's rounded outer corner when the tile sits in one.
                    style={{ borderRadius: ["24px 0 0 0", "0 24px 0 0", "0", "0", "0 0 0 24px", "0 0 24px 0"][i] }}
                  />
                )}
              </div>
            ))}
          </div>
          <div className="pointer-events-none absolute inset-x-0 bottom-0 h-2/5 bg-linear-to-t from-black/80 via-black/40 to-transparent" />
          <div className="absolute inset-x-0 bottom-0 p-8 xl:p-10">
            <p className="max-w-md text-2xl font-bold leading-tight tracking-tight text-white xl:text-3xl">
              Meet, call and message, all in one place.
            </p>
            <p className="mt-2 max-w-md text-sm text-white/80 xl:text-base">
              Video meetings, calls, chats and your contacts, together in CF Meet.
            </p>
          </div>
        </div>
      </aside>
    </div>
  );
}
