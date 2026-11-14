import Link from "next/link";
import { ArrowRight, Check, KeyRound, Webhook } from "lucide-react";
import Reveal from "./Reveal";

// Mirrors sdks/typescript/examples/browser-session.ts.
const code = [
  [["k", "import"], ["t", " { CfMeetClient } "], ["k", "from"], ["s", " '@cfmeet/platform-sdk'"], ["t", ";"]],
  [],
  [["k", "const"], ["t", " client = "], ["k", "new"], ["f", " CfMeetClient"], ["t", "({"]],
  [["t", "  baseUrl: "], ["s", "'https://api.cfmeet.com'"], ["t", ","]],
  [["t", "  auth: { type: "], ["s", "'userSession'"], ["t", ", token },"]],
  [["t", "});"]],
  [],
  [["k", "for await"], ["t", " ("], ["k", "const"], ["t", " org "], ["k", "of"], ["t", " client.developerPlatform"]],
  [["t", "  ."], ["f", "listOrganizations"], ["t", "."], ["f", "all"], ["t", "()) {"]],
  [["t", "  console."], ["f", "log"], ["t", "(org.displayName);"]],
  [["t", "}"], ["c", " ▍"]],
];

const colors: Record<string, string> = {
  k: "text-sky-400",
  s: "text-emerald-300",
  f: "text-amber-200",
  t: "text-slate-300",
  c: "cf-caret text-sky-400",
};

export default function LandingDevelopers() {
  return (
    <section id="developers" aria-labelledby="dev-heading" className="scroll-mt-20 bg-[#0B1220] py-14 text-white sm:py-20">
      <div className="mx-auto grid max-w-7xl items-center gap-12 px-4 sm:px-6 lg:grid-cols-2 lg:gap-16 lg:px-8">
        <Reveal className="min-w-0 max-w-lg">
          <p className="text-sm font-semibold uppercase tracking-[0.18em] text-sky-400">Developers</p>
          <h2 id="dev-heading" className="mt-3 text-3xl font-bold tracking-tight sm:text-5xl">
            Build with CF Meet.
          </h2>
          <p className="mt-5 text-base leading-relaxed text-slate-300 sm:text-lg">
            Bring meetings, calls and messaging into your own product with our APIs, SDKs for TypeScript and
            Dart, and signed webhooks.
          </p>
          <ul className="mt-8 space-y-3 text-slate-200">
            {["Projects with sandbox and production environments", "API keys you control, rotate and revoke", "Webhooks with delivery history and replay"].map((t) => (
              <li key={t} className="flex items-start gap-3">
                <Check className="mt-1 size-4 shrink-0 text-sky-400" />
                {t}
              </li>
            ))}
          </ul>
          <Link
            href="/developers"
            className="group mt-8 inline-flex h-12 items-center gap-2 rounded-xl bg-white px-6 font-semibold text-slate-900 transition-colors hover:bg-slate-100"
          >
            Visit Developer Portal
            <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
          </Link>
        </Reveal>

        <Reveal delay={120} className="relative min-w-0">
          {/* Code window */}
          <div aria-hidden className="overflow-hidden rounded-2xl border border-white/10 bg-[#111A2E] shadow-2xl shadow-black/40">
            <div className="flex items-center gap-2 border-b border-white/10 px-4 py-3">
              <span className="size-3 rounded-full bg-[#FF5F57]" />
              <span className="size-3 rounded-full bg-[#FEBC2E]" />
              <span className="size-3 rounded-full bg-[#28C840]" />
              <span className="ml-3 text-xs text-slate-400">app.ts</span>
            </div>
            <pre className="overflow-x-auto px-4 py-5 font-mono leading-6" style={{ fontSize: "clamp(10.5px, 2.9vw, 13px)" }}>
              {code.map((line, i) => (
                <div key={i} className="flex">
                  <span className="mr-5 w-4 shrink-0 select-none text-right text-slate-600">{i + 1}</span>
                  <span>
                    {line.map(([c, text], j) => (
                      <span key={j} className={colors[c]}>{text}</span>
                    ))}
                  </span>
                </div>
              ))}
            </pre>
          </div>

          {/* API key + webhook card */}
          <div aria-hidden className="mt-4 rounded-2xl border border-white/10 bg-white/[0.04] p-4 backdrop-blur sm:absolute sm:-bottom-10 sm:-right-4 sm:mt-0 sm:w-72 sm:bg-[#16213A]">
            <div className="flex items-center justify-between">
              <span className="flex items-center gap-2 text-sm font-semibold"><KeyRound className="size-4 text-sky-400" /> API key</span>
              <span className="rounded-full bg-emerald-400/15 px-2 py-0.5 text-[11px] font-medium text-emerald-300">Active</span>
            </div>
            <p className="mt-2 font-mono text-xs text-slate-400">cfmk_••••••••••••3f9a</p>
            <div className="mt-3 flex items-center justify-between border-t border-white/10 pt-3">
              <span className="flex items-center gap-2 text-sm font-semibold"><Webhook className="size-4 text-sky-400" /> Webhooks</span>
              <span className="relative h-5 w-9 rounded-full bg-sky-500">
                <span className="absolute right-0.5 top-0.5 size-4 rounded-full bg-white" />
              </span>
            </div>
          </div>
        </Reveal>
      </div>
    </section>
  );
}
