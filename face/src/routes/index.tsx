import { createFileRoute, Link } from "@tanstack/react-router";
import { WaitingList } from "@/components/flare/Waiting";
import { Dot, Key, LinkMark, Section, Trust } from "@/components/flare/marks";
import { SessionStream } from "@/components/flare/Session";
import { agent as sampleAgent, entries, logHead } from "@/lib/mock";
import { useFlare } from "@/components/flare/store";
import { useTitle } from "@/lib/title";
import { clock } from "@/lib/view";

export const Route = createFileRoute("/")({ component: Now });

function Now() {
  useTitle("Now");
  const { waiting, live } = useFlare();
  return (
    <div className="mx-auto grid max-w-[1400px] gap-8 px-4 py-5 md:px-8 md:py-7 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <div className="space-y-8 min-w-0">
        <Section title={`waiting for you · ${waiting.length}`}><WaitingList /></Section>
        {live ? <AgentStatus /> : <SampleAgentStatus />}
        <LogHead />
      </div>
      <div className="space-y-8 min-w-0">
        <RecentActivity />
        <Section title="session · latest" className="hidden xl:block" right={<Link to="/session" className="font-mono text-[11px] text-muted-foreground hover:text-foreground">open →</Link>}>
          <div className="h-[420px]"><SessionStream /></div>
        </Section>
      </div>
    </div>
  );
}

function LogHead() {
  const { live, log, head, link, session } = useFlare();
  if (!live)
    return (
      <Section title="log head" right={<Trust verified={logHead.verified} />}>
        <div className="font-mono text-[12px]">
          <span className="text-[18px] text-foreground">#{head}</span>
          <span className="ml-3 break-all text-muted-foreground">{logHead.hash.slice(0, 16)}…</span>
        </div>
        <p className="mt-1.5 flex items-center gap-1.5 font-mono text-[11px] text-muted-foreground"><Dot tone="verified" /> server reachable · last sync 4s ago</p>
      </Section>
    );
  const view = log.view;
  return (
    <Section title="log head" right={<Trust verified={false} />}>
      <div className="font-mono text-[12px]">
        {view?.head ? (
          <>
            <span className="text-[18px] text-foreground">#{view.head.seq}</span>
            <span className="ml-3 break-all text-muted-foreground" title={view.head.hash}>{view.head.hash.slice(0, 16)}…</span>
          </>
        ) : (
          <span className="text-muted-foreground">{view ? "the log is empty" : log.error ? "not read" : "reading…"}</span>
        )}
      </div>
      <p className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 font-mono text-[11px] text-muted-foreground">
        <LinkMark state={link.state} reason={link.state === "offline" ? link.reason : undefined} />
        <span>session {session}</span>
        {log.readAt && <span>· log read {log.readAt}</span>}
      </p>
      {(view?.problem || log.error) && (
        <p className="mt-1 font-mono text-[11px] text-unverified">
          {log.error ? `The log could not be read: ${log.error}.` : `The server could not bring its copy up to date: ${view?.problem}. This is the log as it last saw it.`}
        </p>
      )}
    </Section>
  );
}

function RecentActivity() {
  const { live, log } = useFlare();
  const rows = live
    ? (log.view?.recent ?? []).map((e) => ({ seq: e.seq, stream: e.stream, kind: e.kind, writer: e.writer.slice(0, 8), label: undefined, time: clock(e.at), content: e.content, verified: false }))
    : entries.slice(0, 14).map((e) => ({ seq: e.seq, stream: e.stream, kind: `${e.kind}${e.evidence ? ` · ${e.evidence}` : ""}`, writer: e.writer.id, label: e.writer.label, time: e.time, content: e.content, verified: e.verified }));
  return (
    <Section title="recent activity">
      {live && rows.length === 0 && <p className="py-6 text-center text-[13px] text-muted-foreground">{log.view ? "The log is empty." : log.error ? "The log could not be read." : "Reading the log…"}</p>}
      <ul className="divide-y divide-border/70">
        {rows.map((e) => (
          <li key={e.seq} className="grid grid-cols-[44px_1fr] gap-x-3 py-1.5">
            <span className="font-mono text-[11.5px] text-muted-foreground">#{e.seq}</span>
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
                <span className="text-foreground/85">{e.stream}</span><span>{e.kind}</span>
                <Key id={e.writer} {...(e.label ? { label: e.label } : {})} />
                <span className="ml-auto flex items-center gap-2">{e.time}<Trust verified={e.verified} /></span>
              </div>
              <p className="truncate text-[13px]" title={e.content}>{e.content}</p>
            </div>
          </li>
        ))}
      </ul>
    </Section>
  );
}

/** What the server last said the agent is doing. */
function AgentStatus() {
  const { agent } = useFlare();
  const working = agent && agent.state !== "idle";
  return (
    <Section title="agent · flare-home" right={<Trust verified={false} />}>
      {!agent ? (
        <p className="text-[13px] text-muted-foreground">The session has no word of the agent yet.</p>
      ) : (
        <>
          <div className="flex items-center gap-2 text-[13.5px]">
            <Dot tone={working ? "primary" : "muted"} pulse={Boolean(working)} /> <span className="font-mono text-[12px]">{agent.state}</span>
            <span className="truncate text-muted-foreground">
              {agent.task ? `task #${agent.task.id}${agent.task.text ? ` · ${agent.task.text}` : ""}` : "no task"} · since {agent.since}
            </span>
          </div>
          {agent.forks.length > 0 && (
            <ul className="mt-3 grid gap-2 sm:grid-cols-2">
              {agent.forks.map((f) => (
                <li key={f.name} className="rounded-sm border border-border bg-card p-2.5">
                  <div className="flex items-center gap-1.5 font-mono text-[11px]"><Dot tone={f.done ? "verified" : "primary"} pulse={!f.done} /><span className="truncate">{f.name}</span><span className="ml-auto text-muted-foreground">{f.done ? "done" : "working"}</span></div>
                  <p className="mt-1 line-clamp-4 whitespace-pre-wrap text-[12.5px] text-muted-foreground">{f.note || "no note seen yet"}</p>
                </li>
              ))}
            </ul>
          )}
          <p className="mt-2 font-mono text-[11px] text-muted-foreground">
            Told from the agent's requests for tasks: a state is the last one the server saw, not a sign of life.
          </p>
        </>
      )}
    </Section>
  );
}

function SampleAgentStatus() {
  const agent = sampleAgent;
  return (
    <Section title="agent · flare-home" right={<span className="font-mono text-[11px] text-muted-foreground">slots left <span className="text-foreground">{agent.slotsLeft}</span>/{agent.slotsTotal}</span>}>
      <div className="flex items-center gap-2 text-[13.5px]">
        <Dot tone="primary" pulse /> <span className="font-mono text-[12px]">forked</span>
        <span className="truncate text-muted-foreground">{agent.task} · turn {agent.turn}</span>
      </div>
      <div className="mt-2 flex gap-1" aria-label="fork slots">
        {Array.from({ length: agent.slotsTotal }).map((_, i) => (
          <span key={i} className={i < agent.slotsTotal - agent.slotsLeft ? "h-1 flex-1 rounded-full bg-muted-foreground/25" : "h-1 flex-1 rounded-full bg-primary/70"} />
        ))}
      </div>
      <ul className="mt-3 grid gap-2 sm:grid-cols-2">
        {agent.forks.map((f) => (
          <li key={f.name} className="rounded-sm border border-border bg-card p-2.5">
            <div className="flex items-center gap-1.5 font-mono text-[11px]"><Dot tone={f.done ? "verified" : "primary"} pulse={!f.done} />{f.name}<span className="ml-auto text-muted-foreground">{f.done ? "done" : "working"}</span></div>
            <p className="mt-1 text-[12.5px] text-muted-foreground">{f.note}</p>
          </li>
        ))}
      </ul>
      <p className="mt-2 font-mono text-[11px] text-muted-foreground">today {agent.usage.calls} calls · {agent.usage.inKB} KB in · {agent.usage.outKB} KB out</p>
    </Section>
  );
}
