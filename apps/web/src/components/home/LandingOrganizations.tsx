import Image from "next/image";
import { Check, ShieldCheck } from "lucide-react";
import Reveal from "./Reveal";

const points = [
  "A shared workspace for your company or school",
  "People and teams, organised your way",
  "Roles and permissions for owners, admins and members",
  "Organization meetings and chats, kept apart from personal ones",
];

const people = [
  { name: "Ada Nwosu", role: "Owner", src: "/landing/avatar-caller.jpg" },
  { name: "Tunde Bello", role: "Admin", src: "/landing/hero-1.jpg" },
  { name: "Amara Okafor", role: "Member", src: "/landing/hero-2.jpg" },
  { name: "Kwame Mensah", role: "Member", src: "/landing/hero-3.jpg" },
];

const teams = ["Engineering", "Design", "Sales", "Support"];

/** A compact organization workspace: header, teams and a people list with roles. */
function WorkspaceMock() {
  return (
    <div aria-hidden className="overflow-hidden rounded-[28px] border border-border bg-card shadow-[0_30px_70px_-30px_rgba(15,23,42,0.35)]">
      <div className="flex items-center justify-between border-b border-border px-5 py-4">
        <div className="flex items-center gap-3">
          <span className="flex size-10 items-center justify-center rounded-xl bg-foreground text-sm font-bold text-background">LH</span>
          <div className="leading-tight">
            <p className="text-sm font-semibold text-foreground">Lagos Health Partners</p>
            <p className="text-xs text-muted-foreground">Organization · 48 members</p>
          </div>
        </div>
        <span className="flex items-center gap-1.5 rounded-full border border-border px-3 py-1 text-xs text-muted-foreground">
          <ShieldCheck className="size-3.5 text-primary" /> Admin
        </span>
      </div>

      <div className="px-5 pt-4">
        <p className="text-xs font-medium text-muted-foreground">Teams</p>
        <div className="mt-2 flex flex-wrap gap-2">
          {teams.map((t, i) => (
            <span
              key={t}
              className={
                i === 0
                  ? "rounded-full bg-foreground px-3 py-1 text-xs font-medium text-background"
                  : "rounded-full border border-border px-3 py-1 text-xs font-medium text-foreground"
              }
            >
              {t}
            </span>
          ))}
        </div>
      </div>

      <div className="px-5 pb-5 pt-5">
        <div className="flex items-center justify-between">
          <p className="text-xs font-medium text-muted-foreground">People</p>
          <span className="rounded-full bg-primary px-3 py-1 text-xs font-semibold text-primary-foreground">Invite</span>
        </div>
        <ul className="mt-2 divide-y divide-border">
          {people.map((p) => (
            <li key={p.name} className="flex items-center gap-3 py-2.5">
              <Image src={p.src} alt="" width={64} height={64} className="size-9 rounded-full object-cover" />
              <div className="min-w-0 flex-1 leading-tight">
                <p className="truncate text-sm font-medium text-foreground">{p.name}</p>
                <p className="text-xs text-muted-foreground">Engineering</p>
              </div>
              <span className="rounded-md bg-muted px-2 py-1 text-[11px] font-medium text-muted-foreground">{p.role}</span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

export default function LandingOrganizations() {
  return (
    <section id="organizations" aria-labelledby="org-heading" className="scroll-mt-20 border-t border-border py-14 sm:py-20">
      <div className="mx-auto grid max-w-7xl items-center gap-14 px-4 sm:px-6 lg:grid-cols-2 lg:gap-20 lg:px-8">
        <Reveal className="max-w-lg lg:order-2">
          <div className="flex flex-wrap items-center gap-3">
            <p className="text-sm font-semibold uppercase tracking-[0.18em] text-primary">Organizations</p>
            <span className="rounded-full border border-border bg-card px-2.5 py-0.5 text-xs font-medium text-muted-foreground">
              Coming soon
            </span>
          </div>
          <h2 id="org-heading" className="mt-3 text-3xl font-bold tracking-tight text-foreground sm:text-5xl">
            Bring your whole organization together.
          </h2>
          <p className="mt-5 text-base leading-relaxed text-muted-foreground sm:text-lg">
            Give your team one place to meet, call and message, with the controls an organization needs.
          </p>
          <ul className="mt-8 space-y-4">
            {points.map((point) => (
              <li key={point} className="flex items-start gap-3 text-base text-foreground">
                <span className="mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary">
                  <Check className="size-3.5" />
                </span>
                {point}
              </li>
            ))}
          </ul>
        </Reveal>

        <Reveal variant="left" delay={120} className="lg:order-1">
          <WorkspaceMock />
        </Reveal>
      </div>
    </section>
  );
}
