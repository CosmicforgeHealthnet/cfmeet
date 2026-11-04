import Link from "next/link";
import { callingEnterpriseLinks } from "@/lib/calling/resource-routes";

export default function CallingEnterpriseHome() {
  return (
    <div className="mx-auto max-w-6xl space-y-5 p-4 md:p-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Enterprise calling</h1>
        <p className="text-sm text-muted-foreground">Manage calling resources within the permissions granted to this account.</p>
      </header>
      <nav className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {callingEnterpriseLinks.map((item) => (
          <Link key={item.href} href={item.href} className="rounded-2xl border border-border bg-card p-5 transition-colors hover:bg-muted">
            <h2 className="font-semibold">{item.label}</h2><p className="mt-1 text-sm text-muted-foreground">{item.description}</p>
          </Link>
        ))}
        <Link href="/calling/enterprise/public-contacts" className="rounded-2xl border border-border bg-card p-5 transition-colors hover:bg-muted">
          <h2 className="font-semibold">Protected destinations</h2><p className="mt-1 text-sm text-muted-foreground">Manage public destinations without exposing raw contact numbers.</p>
        </Link>
      </nav>
    </div>
  );
}
