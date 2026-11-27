import { GraduationCap, HeartPulse, House, Users, UsersRound } from "lucide-react";

const useCases = [
  { label: "Teams", icon: Users },
  { label: "Schools", icon: GraduationCap },
  { label: "Clinics", icon: HeartPulse },
  { label: "Communities", icon: UsersRound },
  { label: "Families", icon: House },
];

/** Who CF Meet is for: a quiet strip between the hero and the feature rows. */
export default function LandingUseCases() {
  return (
    <section aria-label="Who CF Meet is for" className="px-4 sm:px-6 lg:px-8">
      <ul className="mx-auto flex max-w-4xl flex-wrap items-center justify-center gap-x-8 gap-y-5 rounded-3xl border border-border bg-muted/50 px-4 py-6 sm:justify-around sm:gap-6 sm:rounded-full sm:px-8 sm:py-5">
        {useCases.map(({ label, icon: Icon }) => (
          <li key={label} className="flex flex-col items-center gap-2 text-sm font-medium text-foreground sm:flex-row sm:gap-2.5">
            <Icon className="size-5 text-primary" aria-hidden />
            {label}
          </li>
        ))}
      </ul>
    </section>
  );
}
