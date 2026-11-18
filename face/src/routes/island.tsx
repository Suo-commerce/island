import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";

import { Dot, Key, LinkMark, Sample, Trust, Unread } from "@/components/flare/marks";
import { useFlare } from "@/components/flare/store";
import { bytes, inForce, keysOf, ledgerOf, policySteps, when, type KeyStatus } from "@/lib/island";
import type { LogState } from "@/lib/session";
import { useTitle } from "@/lib/title";
import { cn } from "@/lib/utils";

export const Route = createFileRoute("/island")({
  component: Island,
});

const tabs = ["Integrity", "Authority", "Policy", "Gate", "Vessels", "Ledger", "Repos", "Server"] as const;
type Tab = (typeof tabs)[number];

function Island() {
  useTitle("Island");
  const { state, live, log } = useFlare();
  const [tab, setTab] = useState<Tab>("Integrity");
  return (
    <div className="mx-auto max-w-[1100px] px-4 py-6 md:px-10 md:py-10">
      {!live && <Sample className="mb-6" />}
      <header className="mb-8">
        <p className="label-caps">island · the system itself</p>
        <h1 className="mt-1 text-[22px] font-normal tracking-tight">Soundness and rules</h1>
        <p className="mt-1 max-w-xl text-[13px] text-muted-foreground">
          Everything here is replayed from the log by the server. This page changes none of it: grants, revokes and the policy are signed elsewhere, with the root key.
        </p>
      </header>
      <div className="md:grid md:grid-cols-[150px_1fr] md:gap-10">
        <nav className="-mx-4 mb-6 flex gap-1 overflow-x-auto border-b border-border px-4 pb-2 md:mx-0 md:flex-col md:border-0 md:px-0">
          {tabs.map((t) => (
            <button key={t} onClick={() => setTab(t)}
              className={cn("whitespace-nowrap rounded-sm px-2 py-1.5 text-left text-[13px] text-muted-foreground hover:text-foreground", tab === t && "bg-muted text-foreground")}>{t}</button>
          ))}
        </nav>
        <div className="min-w-0">
          {!state ? <Unread error={log.error} /> : (
            <>
              {tab === "Integrity" && <Integrity s={state} />}
              {tab === "Authority" && <Authority s={state} />}
              {tab === "Policy" && <Policy s={state} />}
              {tab === "Gate" && <Gate s={state} />}
              {tab === "Vessels" && <Vessels s={state} />}
              {tab === "Ledger" && <Ledger s={state} />}
              {tab === "Repos" && <Repos s={state} />}
              {tab === "Server" && <Server s={state} />}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function H({ children, right }: { children: React.ReactNode; right?: React.ReactNode }) {
  return <div className="mb-4 flex items-baseline justify-between gap-3 border-b border-border pb-2"><h2 className="text-[15px] font-medium">{children}</h2>{right}</div>;
}
function Row({ k, children }: { k: string; children: React.ReactNode }) {
  return <div className="grid grid-cols-[120px_1fr] gap-3 border-b border-border/60 py-2.5 text-[13px] sm:grid-cols-[200px_1fr]"><span className="text-muted-foreground">{k}</span><span className="min-w-0 break-all">{children}</span></div>;
}
function Note({ children }: { children: React.ReactNode }) {
  return <p className="mb-4 rounded-sm border border-dashed border-border px-3 py-2 text-[12.5px] text-muted-foreground">{children}</p>;
}
function Empty({ children }: { children: React.ReactNode }) {
  return <p className="py-8 text-center text-[13px] text-muted-foreground">{children}</p>;
}
const mono = "font-mono text-[12.5px]";
const said = <Trust verified={false} />;

function Integrity({ s }: { s: LogState }) {
  const kinds = Object.entries(s.kinds).sort((a, b) => b[1] - a[1]);
  return (
    <section>
      <H right={said}>Integrity</H>
      <Note>
        The server keeps its own copy of the log and checks every entry's hash and signatures against the root key before it shows anything. This page has not checked them itself, so it says "server says".
      </Note>
      <Row k="head">{s.head ? <span className={mono}>#{s.head.seq} <span className="text-muted-foreground">{s.head.hash}</span></span> : "the log is empty"}</Row>
      <Row k="entries"><span className={mono}>{s.entries}</span></Row>
      <Row k="root key"><span className={mono}>{s.root}</span></Row>
      <Row k="appender's key"><span className={mono}>{s.appender ?? "none in force"}</span></Row>
      <Row k="state hash"><span className={mono}>{s.state_hash}</span></Row>
      <Row k="server's copy">
        {s.problem
          ? <span className="text-unverified">could not be brought up to date: {s.problem}. This is the log as the server last saw it.</span>
          : <span className="inline-flex items-center gap-1.5"><Dot tone="verified" /> at the broker's head as of {when(s.now)}</span>}
      </Row>
      <h3 className="label-caps mb-2 mt-8">entries by kind</h3>
      <ul className="grid grid-cols-2 gap-x-6 sm:grid-cols-3">
        {kinds.map(([kind, count]) => (
          <li key={kind} className="flex justify-between border-b border-border/60 py-1.5 font-mono text-[12.5px]"><span className="text-muted-foreground">{kind}</span><span>{count}</span></li>
        ))}
      </ul>
      <h3 className="label-caps mb-2 mt-8">streams</h3>
      <ul className="divide-y divide-border/60">
        {s.streams.map((stream) => (
          <li key={stream.name} className="grid grid-cols-[1fr_auto_auto] gap-4 py-1.5 font-mono text-[12.5px]">
            <span>{stream.name}</span>
            <span className="text-muted-foreground">{stream.entries} {stream.entries === 1 ? "entry" : "entries"}</span>
            <span className="text-muted-foreground">newest #{stream.last} · {when(stream.last_at, s.now)}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Authority({ s }: { s: LogState }) {
  const tone: Record<KeyStatus, "muted" | "verified" | "unverified" | "destructive"> = { root: "muted", appender: "muted", active: "verified", narrowed: "unverified", revoked: "destructive" };
  const keys = keysOf(s);
  return (
    <section>
      <H right={said}>Authority</H>
      <Note>A grant can only be narrowed or revoked. Widening is impossible: it takes a new grant, signed with the root key. The log knows keys by their bytes; it holds no names for them.</Note>
      <ul className="divide-y divide-border">
        {keys.map((k) => (
          <li key={k.key} className="grid gap-2 py-3 md:grid-cols-[170px_1fr] md:items-start">
            <div className="flex items-center gap-2">
              <Dot tone={tone[k.status]} /><Key id={k.key.slice(0, 8)} />
              <span className="font-mono text-[11px] text-muted-foreground">{k.status}</span>
            </div>
            <div className="min-w-0 text-[12.5px]">
              {k.holds.length === 0 && <div className="text-muted-foreground">holds no grant in force</div>}
              {k.holds.map((h) => (
                <div key={h.grant}>
                  <span className="text-muted-foreground">may write </span><span className="font-mono">{h.rights.join(", ")}</span>
                  <span className="text-muted-foreground"> on </span><span className="font-mono">{h.scope.join(", ")}</span>
                  <span className="ml-2 font-mono text-[11px] text-muted-foreground">grant {h.grant}</span>
                </div>
              ))}
              <div className="mt-1 font-mono text-[11px] text-muted-foreground">
                {k.history.map((h, i) => <div key={i}>#{h.seq}{h.at === undefined ? "" : ` · ${when(h.at, s.now)}`} · {h.what}</div>)}
                <div>{k.wrote ? `wrote ${k.wrote.entries} ${k.wrote.entries === 1 ? "entry" : "entries"}, last #${k.wrote.last} · ${when(k.wrote.at, s.now)}` : "has written nothing"}</div>
              </div>
              <div className="mt-1 break-all font-mono text-[10.5px] text-muted-foreground/70">{k.key}</div>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Policy({ s }: { s: LogState }) {
  const now = inForce(s.policy);
  const steps = policySteps(s.policy).slice().reverse();
  return (
    <section>
      <H right={said}>Policy</H>
      <Note>The policy only tightens. An entry that would loosen it is refused by the gate and shows under Gate.</Note>
      <h3 className="label-caps mb-2">in force</h3>
      {now.length === 0 ? <Empty>No policy entry yet: nothing is required of a verse.</Empty> : (
        <ul className="divide-y divide-border/60">
          {now.map((rule) => (
            <li key={rule.invariant} className="flex items-baseline justify-between gap-3 py-2">
              <span className={mono}>{rule.invariant}</span>
              <span className="font-mono text-[11px] text-muted-foreground">since #{rule.since}</span>
            </li>
          ))}
        </ul>
      )}
      <h3 className="label-caps mb-2 mt-8">history</h3>
      <ul className="divide-y divide-border/60">
        {steps.map((step) => (
          <li key={step.seq} className="grid grid-cols-[52px_1fr] gap-x-3 py-2">
            <span className="font-mono text-[11.5px] text-muted-foreground">#{step.seq}</span>
            <div className="min-w-0">
              {step.changes.map((change) => <p key={change} className={mono}>{change}</p>)}
              <p className="font-mono text-[11px] text-muted-foreground">{when(step.at, s.now)} · judged by build {step.evaluator.slice(0, 7) || "unnamed"}</p>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Gate({ s }: { s: LogState }) {
  return (
    <section>
      <H right={<span className="font-mono text-[11px] text-muted-foreground">{s.refused} refused in all</span>}>Gate</H>
      <Note>Every submission the gate refused is kept in the log as a verdict, with what was refused and why.</Note>
      {s.verdicts.length === 0 ? <Empty>The gate has refused nothing.</Empty> : (
        <ul className="divide-y divide-border">
          {s.verdicts.map((v) => (
            <li key={v.seq} className="grid grid-cols-[52px_1fr] gap-x-3 py-2.5">
              <span className="font-mono text-[11.5px] text-muted-foreground">#{v.seq}</span>
              <div className="min-w-0">
                <p className="text-[13px] text-destructive">{v.reason}</p>
                <p className="truncate text-[12.5px] text-muted-foreground" title={v.content}>{v.content}</p>
                <p className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
                  <span>{v.kind} for {v.stream}</span><Key id={v.writer.slice(0, 8)} /><span>{when(v.at, s.now)}</span>
                </p>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function Vessels({ s }: { s: LogState }) {
  return (
    <section>
      <H right={said}>Vessels</H>
      <Note>What each Vessel was given through the island socket, counted from the log's access entries, and how many of its actions were let out. The log knows a Vessel by its id, not by a name.</Note>
      {s.vessels.length === 0 ? <Empty>No Vessel has an access or dispatch entry yet.</Empty> : (
        <ul className="space-y-6">
          {s.vessels.map((v) => (
            <li key={v.vessel}>
              <p className="mb-2 flex flex-wrap items-baseline gap-x-3 font-mono text-[12.5px]">
                <span>Vessel {v.vessel.slice(0, 8)}</span>
                <span className="break-all text-[10.5px] text-muted-foreground/70">{v.vessel}</span>
              </p>
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                {[
                  [`${v.inference_today.calls}`, "model calls today (UTC)"],
                  [`${v.inference.calls}`, "model calls ever"],
                  [bytes(v.inference.bytes_in), "sent to models"],
                  [bytes(v.inference.bytes_out), "received from models"],
                  [`${v.tasks.calls}`, "tasks pulled"],
                  [`${v.notes.calls}`, "fork notes"],
                  [`${v.reads.calls}`, "reads"],
                  [`${v.dispatched}`, "actions let out"],
                ].map(([value, label]) => (
                  <div key={label} className="rounded-sm border border-border bg-card p-2.5">
                    <div className="font-mono text-[14px]">{value}</div>
                    <div className="font-mono text-[10.5px] text-muted-foreground">{label}</div>
                  </div>
                ))}
              </div>
              <p className="mt-2 font-mono text-[11px] text-muted-foreground">
                {Object.entries(v.providers).map(([name, used]) => `${name || "unnamed"}: ${used.calls} calls`).join(" · ") || "no model call yet"}
                {v.last_access !== null && v.last_access_at !== null && ` · newest access entry #${v.last_access} · ${when(v.last_access_at, s.now)}`}
              </p>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function Ledger({ s }: { s: LogState }) {
  const rows = ledgerOf(s);
  return (
    <section>
      <H right={said}>Ledger</H>
      <Note>Every answer to an action that waited, and the dispatch that used it. An approval lets one action out, once.</Note>
      {rows.length === 0 ? <Empty>Nothing has been approved or declined yet.</Empty> : (
        <ul className="divide-y divide-border">
          {rows.map(({ approval, dispatch, outcome }) => (
            <li key={approval.seq} className="grid grid-cols-[52px_1fr] gap-x-3 py-2.5">
              <span className="font-mono text-[11.5px] text-muted-foreground">#{approval.seq}</span>
              <div className="min-w-0">
                <p className="flex flex-wrap items-center gap-x-2 text-[13px]">
                  <span className={outcome === "sent" ? "text-verified" : outcome === "declined" ? "text-destructive" : "text-unverified"}>{outcome}</span>
                  <span className="font-mono text-[12px] text-muted-foreground">req {approval.request.slice(0, 8)}</span>
                </p>
                <p className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
                  <span>{approval.verdict === "approve" ? "approved" : "declined"} by</span><Key id={approval.by.slice(0, 8)} /><span>{when(approval.at, s.now)}</span>
                  {dispatch && <span>· dispatch #{dispatch.seq}, op {dispatch.op} of Vessel {dispatch.vessel.slice(0, 8)}</span>}
                  {!dispatch && approval.used_by !== null && <span>· dispatch #{approval.used_by}</span>}
                </p>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function Repos({ s }: { s: LogState }) {
  const receipts = s.kinds["receipt"] ?? 0;
  return (
    <section>
      <H>Repos</H>
      <Empty>
        {receipts > 0
          ? `The log holds ${receipts} ${receipts === 1 ? "receipt" : "receipts"}, but this screen does not show them yet.`
          : "Nothing watches your repos yet: the observer that writes receipts for commits is the next build step (step 7). The log holds no receipt."}
      </Empty>
    </section>
  );
}

function Server({ s }: { s: LogState }) {
  const { link, session, client, log } = useFlare();
  const skew = Math.round((Date.now() * 1000 - s.now) / 1_000_000);
  return (
    <section>
      <H>Server</H>
      <Row k="session"><span className="inline-flex flex-wrap items-center gap-x-2"><LinkMark state={link.state} reason={link.state === "offline" ? link.reason : undefined} /><span className={mono}>{session}, as {client}</span></span></Row>
      <Row k="the server's copy of the log">{s.problem ? <span className="text-unverified">behind: {s.problem}</span> : <span className="inline-flex items-center gap-1.5"><Dot tone="verified" /> up to date with the broker</span>}</Row>
      <Row k="log last read"><span className={mono}>{log.readAt ?? when(s.now)}</span></Row>
      <Row k="server's clock"><span className={mono}>{when(s.now)}{Math.abs(skew) > 90 ? `, ${Math.abs(skew)} s ${skew > 0 ? "behind" : "ahead of"} this device's` : ""}</span></Row>
      <h3 className="label-caps mb-2 mt-8">keys that wrote, newest first</h3>
      <ul className="divide-y divide-border/60">
        {s.writers.map((w) => (
          <li key={w.key} className="grid grid-cols-[auto_1fr_auto] items-center gap-4 py-1.5 font-mono text-[12.5px]">
            <Key id={w.key.slice(0, 8)} />
            <span className="text-muted-foreground">{w.entries} {w.entries === 1 ? "entry" : "entries"}</span>
            <span className="text-muted-foreground">last #{w.last} · {when(w.last_at, s.now)}</span>
          </li>
        ))}
      </ul>
      <p className="mt-4 text-[12.5px] text-muted-foreground">
        Whether the services on the server are running is not something the log says. On the server, <span className="font-mono">scripts/node/check.sh</span> does.
      </p>
    </section>
  );
}
