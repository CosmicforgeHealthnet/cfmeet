/**
 * Small two-tone illustrations for Home empty states, drawn in the brand blue so they
 * read as CF Meet rather than generic icons. Decorative: always aria-hidden.
 */

export function CalendarArt({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 56 56" fill="none" className={className} aria-hidden>
      <rect x="6" y="10" width="44" height="40" rx="9" className="fill-primary/10 stroke-primary/40" strokeWidth="1.5" />
      <path d="M6 19a9 9 0 0 1 9-9h26a9 9 0 0 1 9 9v3H6v-3Z" className="fill-primary/25" />
      <rect x="17" y="6" width="3.5" height="9" rx="1.75" className="fill-primary" />
      <rect x="35.5" y="6" width="3.5" height="9" rx="1.75" className="fill-primary" />
      {[0, 1, 2, 3].map((c) =>
        [0, 1].map((r) => (
          <rect key={`${c}-${r}`} x={13 + c * 8.5} y={28 + r * 8.5} width="5" height="5" rx="1.5" className="fill-primary/25" />
        )),
      )}
      <circle cx="40" cy="40" r="8" className="fill-primary" />
      <path d="m36.5 40 2.4 2.4 4.6-4.8" stroke="white" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function RoomArt({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 56 56" fill="none" className={className} aria-hidden>
      <rect x="5" y="11" width="40" height="30" rx="8" className="fill-primary/10 stroke-primary/40" strokeWidth="1.5" />
      <circle cx="25" cy="23" r="5" className="fill-primary/40" />
      <path d="M15.5 37c1.6-4.6 5.2-7 9.5-7s7.9 2.4 9.5 7" className="fill-primary/25" />
      <rect x="11" y="15" width="7" height="3" rx="1.5" className="fill-primary/30" />
      <circle cx="42" cy="40" r="9" className="fill-primary" />
      <path
        d="M40.2 41.8 43.8 38.2M39.6 39l-1 1a2.4 2.4 0 0 0 3.4 3.4l1-1M44.4 41l1-1a2.4 2.4 0 0 0-3.4-3.4l-1 1"
        stroke="white"
        strokeWidth="1.7"
        strokeLinecap="round"
      />
    </svg>
  );
}
