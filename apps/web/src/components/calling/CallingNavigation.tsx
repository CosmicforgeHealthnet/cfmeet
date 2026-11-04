import Link from "next/link";

const links = [
  ["Calling", "/calling"],
  ["History", "/calling/history"],
  ["Devices", "/calling/devices"],
  ["Settings", "/calling/settings"],
  ["Privacy", "/calling/privacy/contexts"],
  ["Enterprise", "/calling/enterprise"],
  ["Webhooks", "/calling/developers/webhooks"],
  ["Usage", "/calling/usage"],
  ["Support", "/calling/support"],
];

export default function CallingNavigation() {
  return (
    <nav aria-label="Calling navigation" className="border-b border-border bg-background px-4 py-2 md:px-6">
      <ul className="flex gap-2 overflow-x-auto">
        {links.map(([label, href]) => <li key={href}><Link href={href} className="block whitespace-nowrap rounded-lg px-3 py-2 text-sm text-muted-foreground hover:bg-muted hover:text-foreground">{label}</Link></li>)}
      </ul>
    </nav>
  );
}
