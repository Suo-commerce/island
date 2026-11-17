import { createFileRoute } from "@tanstack/react-router";
import { useTitle } from "@/lib/title";
import { Dot, Key, Sample, Section } from "@/components/flare/marks";
import { agentBudget, agentConfigHistory, agentIdentity, agentStreams, outwardPolicy } from "@/lib/agent-mock";

export const Route = createFileRoute("/agent")({
  component: AgentPage,
});

function AgentPage() {
  useTitle("Agent");
  const b = agentBudget;
  return (
    <div className="mx-auto grid max-w-[1400px] gap-8 px-4 py-5 md:px-8 md:py-7 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <Sample what="The agent's settings" className="xl:col-span-2" />
      <div className="min-w-0 space-y-8">
        <Section title="identity" right={<span className="font-mono text-[11px] text-muted-foreground">granted #{agentIdentity.grantedSeq}</span>}>
          <div className="flex items-center gap-2 text-[13.5px]">
            <Dot tone="primary" pulse />
            <span className="font-mono text-[12px]">{agentIdentity.state}</span>
            <span className="truncate text-muted-foreground">{agentIdentity.task} · turn {agentIdentity.turn}</span>
          </div>
          <div className="mt-2 flex items-center gap-2 font-mono text-[11px] text-muted-foreground">
            key <Key id={agentIdentity.key} label={agentIdentity.name} />
            <span className="ml-auto">granted {agentIdentity.grantedWhen} by {agentIdentity.grantedBy.label}</span>
          </div>
        </Section>

        <Section title="outward policy">
          <ul className="divide-y divide-border/70">
            {outwardPolicy.map((p) => (
              <li key={p.kind} className="grid grid-cols-[72px_1fr] gap-x-3 py-1.5">
                <span className="font-mono text-[12px] text-foreground/85">{p.kind}</span>
                <div className="min-w-0">
                  <span className="font-mono text-[12px] text-primary">{p.rule}</span>
                  <p className="text-[12.5px] text-muted-foreground">{p.note} · since #{p.since}</p>
                </div>
              </li>
            ))}
          </ul>
        </Section>

        <Section title="budget · today" right={<span className="font-mono text-[11px] text-muted-foreground">slots <span className="text-foreground">{b.slotsTotal - b.slotsUsed}</span>/{b.slotsTotal} left</span>}>
          <div className="flex gap-1" aria-label="fork slots">
            {Array.from({ length: b.slotsTotal }).map((_, i) => (
              <span key={i} className={i < b.slotsUsed ? "h-1 flex-1 rounded-full bg-muted-foreground/25" : "h-1 flex-1 rounded-full bg-primary/70"} />
            ))}
          </div>
          <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
            {[
              [`${b.callsToday}/${b.callsCap}`, "model calls"],
              [`${b.inKB} KB`, "in"],
              [`${b.outKB}/${b.outCapKB} KB`, "out"],
              [`${b.maxTurns}`, "max turns"],
            ].map(([v, l]) => (
              <div key={l} className="rounded-sm border border-border bg-card p-2.5">
                <div className="font-mono text-[14px]">{v}</div>
                <div className="font-mono text-[10.5px] text-muted-foreground">{l}</div>
              </div>
            ))}
          </div>
          <p className="mt-2 font-mono text-[11px] text-muted-foreground">max forks {b.maxForks} · adopting a fork needs {b.forkAdopt}</p>
        </Section>
      </div>

      <div className="min-w-0 space-y-8">
        <Section title="streams">
          <ul className="divide-y divide-border/70">
            {agentStreams.map((s) => (
              <li key={s.stream} className="flex items-center gap-3 py-1.5 font-mono text-[12px]">
                <span className="w-14 text-foreground/85">{s.stream}</span>
                <span className="text-muted-foreground">read {s.read ? "yes" : "no"}</span>
                <span className={s.write === "no" ? "ml-auto text-muted-foreground/60" : s.write === "auto" ? "ml-auto text-primary" : "ml-auto text-muted-foreground"}>write {s.write}</span>
              </li>
            ))}
          </ul>
        </Section>

        <Section title="configuration history">
          <ul className="divide-y divide-border/70">
            {agentConfigHistory.map((h) => (
              <li key={h.seq} className="grid grid-cols-[52px_1fr] gap-x-3 py-1.5">
                <span className="font-mono text-[11.5px] text-muted-foreground">#{h.seq}</span>
                <div className="min-w-0">
                  <p className="text-[13px]">{h.text}</p>
                  <p className="font-mono text-[11px] text-muted-foreground">{h.when}</p>
                </div>
              </li>
            ))}
          </ul>
        </Section>
      </div>
    </div>
  );
}
