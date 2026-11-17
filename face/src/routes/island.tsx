import { createFileRoute } from "@tanstack/react-router";
import { useTitle } from "@/lib/title";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Dot, Key, Sample, Trust } from "@/components/flare/marks";
import { entries } from "@/lib/mock";
import * as I from "@/lib/island-mock";
import { cn } from "@/lib/utils";

export const Route = createFileRoute("/island")({
  component: Island,
});

const tabs = ["Integrity", "Authority", "Policy", "Gate", "Vessels", "Ledger", "Repos", "Server"] as const;
type Tab = (typeof tabs)[number];

function Island() {
  useTitle("Island");
  const [tab, setTab] = useState<Tab>("Integrity");
  return (
    <div className="mx-auto max-w-[1100px] px-4 py-6 md:px-10 md:py-10">
      <Sample what="Island" className="mb-6" />
      <header className="mb-8">
        <p className="label-caps">island · the system itself</p>
        <h1 className="mt-1 text-[22px] font-normal tracking-tight">Soundness and rules</h1>
        <p className="mt-1 max-w-xl text-[13px] text-muted-foreground">Things here change rarely. Root actions are signed elsewhere with the offline root key.</p>
      </header>
      <div className="md:grid md:grid-cols-[150px_1fr] md:gap-10">
        <nav className="-mx-4 mb-6 flex gap-1 overflow-x-auto border-b border-border px-4 pb-2 md:mx-0 md:flex-col md:border-0 md:px-0">
          {tabs.map((t) => (
            <button key={t} onClick={() => setTab(t)}
              className={cn("whitespace-nowrap rounded-sm px-2 py-1.5 text-left text-[13px] text-muted-foreground hover:text-foreground", tab === t && "bg-muted text-foreground")}>{t}</button>
          ))}
        </nav>
        <div className="min-w-0">
          {tab === "Integrity" && <Integrity />}
          {tab === "Authority" && <Authority />}
          {tab === "Policy" && <Policy />}
          {tab === "Gate" && <Gate />}
          {tab === "Vessels" && <Vessels />}
          {tab === "Ledger" && <Ledger />}
          {tab === "Repos" && <Repos />}
          {tab === "Server" && <Server />}
        </div>
      </div>
    </div>
  );
}

function H({ children, right }: { children: React.ReactNode; right?: React.ReactNode }) {
  return <div className="mb-4 flex items-baseline justify-between border-b border-border pb-2"><h2 className="text-[15px] font-medium">{children}</h2>{right}</div>;
}
function Row({ k, children }: { k: string; children: React.ReactNode }) {
  return <div className="grid grid-cols-[140px_1fr] gap-3 border-b border-border/60 py-2.5 text-[13px] sm:grid-cols-[200px_1fr]"><span className="text-muted-foreground">{k}</span><span className="min-w-0 break-all">{children}</span></div>;
}
const mono = "font-mono text-[12.5px]";

function Integrity() {
  const [p, setP] = useState<number | null>(null);
  const [at, setAt] = useState(I.integrity.verifiedAt);
  useEffect(() => {
    if (p === null || p >= 100) return;
    const t = setTimeout(() => setP((x) => Math.min(100, (x ?? 0) + 7)), 90);
    return () => clearTimeout(t);
  }, [p]);
  useEffect(() => { if (p === 100) setAt("just now"); }, [p]);
  const running = p !== null && p < 100;
  return (
    <section>
      <H right={<Button size="sm" variant="outline" disabled={running} onClick={() => setP(0)}>{running ? "verifying…" : "Verify again"}</Button>}>Integrity</H>
      {p !== null && (
        <div className="mb-4">
          <div className="h-1 overflow-hidden rounded-full bg-muted"><div className="h-full bg-verified transition-all" style={{ width: `${p}%` }} /></div>
          <p className="mt-1 font-mono text-[11px] text-muted-foreground">{running ? `checking entry ${Math.round((p / 100) * I.integrity.head)} of ${I.integrity.head}` : `all ${I.integrity.head} signatures and links hold`}</p>
        </div>
      )}
      <Row k="log head"><span className={mono}>#{I.integrity.head}</span></Row>
      <Row k="head hash"><span className={mono}>{I.integrity.headHash}</span></Row>
      <Row k="verification"><span className="flex items-center gap-2"><Trust verified /> from genesis on this device · {at}</span></Row>
      <Row k="pinned root key"><span className={mono}>{I.integrity.rootKey}</span></Row>
      <Row k="appender key"><Key id={I.integrity.appender} label="observer" /></Row>
      <Row k="state hash"><span className={mono}>{I.integrity.stateHash}</span></Row>
    </section>
  );
}

function RootSign({ open, onOpenChange, what }: { open: boolean; onOpenChange: (o: boolean) => void; what: string }) {
  const [sig, setSig] = useState("");
  const hash = "a71f03e9c2d84b56e0f1937ab2c4d58e";
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="text-[15px] font-medium">Sign with root key</DialogTitle>
          <DialogDescription>{what}. This needs the offline root key <span className="font-mono">11c0ffee</span>; sign the hash on that device and paste the signature here.</DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <div><p className="label-caps mb-1">1 · copy hash</p>
            <div className="flex gap-2"><code className="flex-1 truncate rounded-sm border border-border bg-muted/50 px-2 py-1.5 font-mono text-[12px]">{hash}</code>
              <Button size="sm" variant="outline" onClick={() => navigator.clipboard?.writeText(hash)}>Copy</Button></div></div>
          <div><p className="label-caps mb-1">2 · paste signature</p>
            <textarea value={sig} onChange={(e) => setSig(e.target.value)} rows={3} placeholder="ed25519 signature (hex)" className="w-full rounded-sm border border-input bg-card p-2 font-mono text-[12px] focus:outline-none focus:ring-1 focus:ring-ring" /></div>
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>Cancel</Button>
          <Button disabled={sig.trim().length < 16} onClick={() => onOpenChange(false)}>Submit to log</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function Authority() {
  const [dlg, setDlg] = useState<string | null>(null);
  const tone = { root: "muted", active: "verified", narrowed: "unverified", revoked: "destructive" } as const;
  return (
    <section>
      <H right={<Button size="sm" variant="outline" onClick={() => setDlg("New root grant for a key")}>Grant… <span className="ml-1 font-mono text-[10px] text-muted-foreground">root</span></Button>}>Authority</H>
      <p className="mb-4 rounded-sm border border-dashed border-border px-3 py-2 text-[12.5px] text-muted-foreground">
        A grant can only be narrowed. Widening is impossible; it takes a new root grant signed with the root key.
      </p>
      <ul className="divide-y divide-border">
        {I.keys.map((k) => (
          <li key={k.id} className="grid gap-2 py-3 md:grid-cols-[200px_1fr_auto] md:items-start">
            <div className="flex items-center gap-2"><Dot tone={tone[k.status]} /><Key id={k.id} label={k.label} /></div>
            <div className="text-[12.5px]">
              <div><span className="text-muted-foreground">streams </span><span className="font-mono">{k.streams}</span></div>
              <div><span className="text-muted-foreground">kinds </span><span className="font-mono">{k.kinds}</span></div>
              <div className="mt-1 font-mono text-[11px] text-muted-foreground">
                {k.history.map((h) => <span key={h.seq} className="mr-3">{h.what} #{h.seq} · {h.when}</span>)}
              </div>
            </div>
            {k.status !== "root" && k.status !== "revoked" && (
              <div className="flex gap-1.5">
                <Button size="sm" variant="ghost" onClick={() => setDlg(`Narrow grant for ${k.label}`)}>Narrow</Button>
                <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setDlg(`Revoke ${k.label}`)}>Revoke</Button>
                <Button size="sm" variant="ghost" disabled title="Widening is impossible; issue a new root grant">Widen</Button>
              </div>
            )}
          </li>
        ))}
      </ul>
      <RootSign open={!!dlg} onOpenChange={(o) => !o && setDlg(null)} what={dlg ?? ""} />
    </section>
  );
}

function Policy() {
  const [proposal, setProposal] = useState("");
  const would = proposal.trim().length > 3
    ? entries.filter((e) => e.kind === "verse" && (proposal.match(/\d+/) ? e.content.length > Number(proposal.match(/\d+/)![0]) : !e.evidence || e.content.length > 60)).slice(0, 6)
    : [];
  return (
    <section className="space-y-8">
      <div>
        <H right={<span className="font-mono text-[11px] text-muted-foreground">override requests <span className="text-foreground">{I.overrideCount.total}</span> total · {I.overrideCount.last30} in 30 days</span>}>Rules in force</H>
        <ul className="divide-y divide-border/60">
          {I.rules.map((r) => (
            <li key={r.id} className="flex items-baseline gap-3 py-2 text-[13.5px]">
              <span className="w-6 font-mono text-[11px] text-muted-foreground">{r.id}</span><span className="flex-1">{r.text}</span>
              <span className="font-mono text-[11px] text-muted-foreground">since #{r.since}</span>
            </li>
          ))}
        </ul>
      </div>
      <div>
        <H>Ratchet · only ever tightens</H>
        <ol className="relative ml-1.5 border-l border-border">
          {I.policyHistory.map((h) => (
            <li key={h.seq} className="relative py-2 pl-5">
              <span className={cn("absolute top-[13px] -left-[4px] size-[7px] rounded-full", h.kind === "tighten" ? "bg-foreground/70" : "border border-unverified bg-background")} />
              <div className="flex flex-wrap items-baseline gap-x-3">
                <span className="font-mono text-[11px] text-muted-foreground">#{h.seq} · {h.when}</span>
                <span className={cn("text-[13px]", h.kind === "refused" && "text-unverified line-through decoration-1")}>{h.text}</span>
              </div>
            </li>
          ))}
        </ol>
      </div>
      <div>
        <H>Propose a tightening</H>
        <input value={proposal} onChange={(e) => setProposal(e.target.value)} placeholder='e.g. "verse text at most 80 bytes"'
          className="h-10 w-full rounded-sm border border-input bg-card px-3 text-[13.5px] focus:outline-none focus:ring-1 focus:ring-ring" />
        {would.length > 0 && (
          <div className="mt-3">
            <p className="mb-1 text-[12.5px] text-muted-foreground">Would have refused {would.length} past entries. History is not changed; this applies from the next entry on.</p>
            <ul className="font-mono text-[12px]">
              {would.map((e) => <li key={e.seq} className="truncate py-0.5"><span className="text-muted-foreground">#{e.seq} {e.stream} </span>{e.content}</li>)}
            </ul>
            <Button size="sm" className="mt-3" variant="outline">Submit proposal (laptop key)</Button>
          </div>
        )}
      </div>
    </section>
  );
}

function Gate() {
  return (
    <section>
      <H>Recent verdicts</H>
      <ul className="divide-y divide-border">
        {I.verdicts.map((v) => (
          <li key={v.seq} className="grid grid-cols-[52px_1fr] gap-3 py-2.5 sm:grid-cols-[52px_90px_1fr_auto]">
            <span className="font-mono text-[12px] text-muted-foreground">#{v.seq}</span>
            <span className="font-mono text-[12px]">{v.stream}</span>
            <span className="col-span-2 text-[13px] sm:col-span-1"><span className="mr-2 font-mono text-[11px] text-unverified">{v.rule}</span>{v.text}</span>
            <span className="col-span-2 font-mono text-[11px] text-muted-foreground sm:col-span-1">{v.writer.label} · {v.when}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Vessels() {
  return (
    <section>
      <H>Agents and Vessels</H>
      <div className="grid gap-3 md:grid-cols-3">
        {I.vessels.map((v) => (
          <div key={v.name} className="rounded-sm border border-border bg-card p-3 text-[12.5px]">
            <div className="mb-2 flex items-center gap-2 font-mono text-[12px]"><Dot tone={v.state === "done" ? "verified" : "primary"} pulse={v.state !== "done"} />{v.name}</div>
            <dl className="grid grid-cols-[90px_1fr] gap-y-1">
              <dt className="text-muted-foreground">state</dt><dd>{v.state}</dd>
              {v.slotsTotal > 0 && (<><dt className="text-muted-foreground">slots</dt><dd className="font-mono">{v.slotsUsed} used · {v.slotsTotal - v.slotsUsed} left</dd></>)}
              <dt className="text-muted-foreground">last seal</dt><dd className="font-mono">{v.seal}</dd>
              <dt className="text-muted-foreground">effects</dt><dd className={v.authority.startsWith("blocked") ? "text-unverified" : ""}>{v.authority}</dd>
              <dt className="text-muted-foreground">model today</dt><dd className="font-mono">{v.calls}{v.budget ? ` / ${v.budget}` : ""} calls</dd>
            </dl>
            {v.budget > 0 && <div className="mt-2 h-1 rounded-full bg-muted"><div className="h-full rounded-full bg-foreground/50" style={{ width: `${(v.calls / v.budget) * 100}%` }} /></div>}
          </div>
        ))}
      </div>
    </section>
  );
}

function Ledger() {
  return (
    <section>
      <H right={<span className="font-mono text-[11px] text-verified">0 sent twice</span>}>Approvals ledger</H>
      <ul className="divide-y divide-border">
        {I.ledger.map((l) => (
          <li key={l.approval + l.outcome} className="flex flex-wrap items-center gap-x-2 gap-y-1 py-2.5 font-mono text-[12px]">
            <span className={cn("w-16", l.outcome === "approved" ? "text-verified" : l.outcome === "lapsed" ? "text-unverified" : "text-muted-foreground")}>{l.outcome}</span>
            <span>#{l.approval}</span><span className="text-muted-foreground">{l.by}</span>
            <span className="text-muted-foreground">→</span><span>{l.action}</span>
            <span className="text-muted-foreground">→</span><span>{l.entry ? `entry #${l.entry}` : "—"}</span>
            <span className="w-full font-sans text-[12px] text-muted-foreground sm:ml-auto sm:w-auto">{l.note}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Repos() {
  return (
    <section>
      <H>Repos</H>
      <table className="w-full text-left text-[13px]">
        <thead className="label-caps"><tr><th className="pb-2 font-normal">repo → stream</th><th className="font-normal">branch</th><th className="font-normal">latest receipt</th><th className="text-right font-normal">uncovered</th></tr></thead>
        <tbody className="divide-y divide-border/60">
          {I.repos.map((r) => (
            <tr key={r.repo}><td className="py-2 font-mono text-[12.5px]">{r.repo} → {r.stream}</td><td className="font-mono text-[12.5px]">{r.branch}</td>
              <td className="font-mono text-[12.5px]">{r.head} <span className="text-muted-foreground">#{r.seq}</span></td>
              <td className={cn("text-right font-mono", r.uncovered ? "text-unverified" : "text-muted-foreground")}>{r.uncovered}</td></tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

function Server() {
  return (
    <section className="space-y-8">
      <div><H>Services</H>
        <ul className="grid gap-2 sm:grid-cols-2">
          {I.services.map((s) => (
            <li key={s.name} className="flex items-center gap-2 rounded-sm border border-border px-3 py-2 text-[13px]">
              <Dot tone={s.up ? "verified" : "destructive"} pulse={!s.up} /><span>{s.name}</span><span className="ml-auto font-mono text-[11px] text-muted-foreground">{s.up ? s.note : s.note}</span>
            </li>
          ))}
        </ul>
      </div>
      <div><H>Devices in the private network</H>
        <table className="w-full text-left text-[13px]">
          <thead className="label-caps"><tr><th className="pb-2 font-normal">device</th><th className="font-normal">last seen</th><th className="font-normal">backup / replica</th></tr></thead>
          <tbody className="divide-y divide-border/60">
            {I.devices.map((d) => (
              <tr key={d.label}><td className="py-2"><Key id={d.key} label={d.label} /></td><td className="font-mono text-[12px]">{d.seen}</td><td className="font-mono text-[12px]">{d.backup}</td></tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}
