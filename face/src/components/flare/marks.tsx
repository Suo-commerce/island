import { cn } from "@/lib/utils";

export function Trust({ verified, className }: { verified: boolean; className?: string }) {
  return verified ? (
    <span title="Checked on this device against the log's signatures" className={cn("inline-flex items-center gap-1 font-mono text-[11px] text-verified", className)}>
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden><path d="M1.5 5.2 4 7.5 8.5 2.5" fill="none" stroke="currentColor" strokeWidth="1.6" /></svg>
      verified
    </span>
  ) : (
    <span title="Not checked locally; the server says so" className={cn("inline-flex items-center gap-1 font-mono text-[11px] text-unverified", className)}>
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden><circle cx="5" cy="5" r="3.5" fill="none" stroke="currentColor" strokeWidth="1.2" strokeDasharray="2 1.5" /></svg>
      server says
    </span>
  );
}

export function Dot({ tone = "muted", pulse }: { tone?: "primary" | "verified" | "muted" | "unverified" | "destructive"; pulse?: boolean }) {
  const c = { primary: "bg-primary", verified: "bg-verified", muted: "bg-muted-foreground/50", unverified: "bg-unverified", destructive: "bg-destructive" }[tone];
  return <span className={cn("inline-block size-1.5 shrink-0 rounded-full", c, pulse && "animate-pulse")} />;
}

export function Key({ id, label }: { id: string; label?: string }) {
  return (
    <span className="whitespace-nowrap">
      <span className="font-mono text-[11.5px] text-muted-foreground">{id}</span>
      {label && <span className="ml-1 text-[12px] text-foreground/80">{label}</span>}
    </span>
  );
}

export function Section({ title, right, children, className }: { title: string; right?: React.ReactNode; children: React.ReactNode; className?: string }) {
  return (
    <section className={className}>
      <div className="mb-2 flex items-baseline justify-between border-b border-border pb-1.5">
        <h2 className="label-caps">{title}</h2>
        {right}
      </div>
      {children}
    </section>
  );
}

/** Said on every screen that is not connected yet: what it shows is not Island. */
export function Sample({ what, className }: { what: string; className?: string }) {
  return (
    <p role="note" className={cn("rounded-sm border border-dashed border-unverified/60 bg-unverified/10 px-3 py-2 font-mono text-[11.5px] text-foreground/80", className)}>
      <span className="text-unverified">sample data</span> · {what} is not connected yet. Nothing here is read from Island, and nothing done here is sent.
    </p>
  );
}

/** Whether the page is attached to its session. */
export function LinkMark({ state, reason }: { state: "connecting" | "attached" | "offline"; reason?: string | undefined }) {
  if (state === "attached") return <span className="inline-flex items-center gap-1.5"><Dot tone="verified" /> attached</span>;
  if (state === "connecting") return <span className="inline-flex items-center gap-1.5"><Dot tone="muted" pulse /> attaching…</span>;
  return <span className="inline-flex items-center gap-1.5" title={reason}><Dot tone="destructive" /> not attached</span>;
}
