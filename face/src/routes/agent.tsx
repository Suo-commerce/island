import { createFileRoute } from "@tanstack/react-router";

import { Dot, Key, Sample, Section, Trust, Unread } from "@/components/flare/marks";
import { useFlare } from "@/components/flare/store";
import { bytes, count, ledgerOf, when } from "@/lib/island";
import { useTitle } from "@/lib/title";

export const Route = createFileRoute("/agent")({
  component: AgentPage,
});

/** How this build treats what the agent sends. It is the build's rule, not a setting in the log. */
const outward = [
  { kind: "say", rule: "sent without asking", note: "its replies to the session go out as soon as it seals its turn" },
  { kind: "verse", rule: "waits for you", note: "every verse waits for an approval, and is written once, signed with the Vessel's key" },
  { kind: "thinking", rule: "counted", note: "every model call goes through the island socket and into the access stream as counts and digests" },
  { kind: "fork", rule: "its own budget", note: "it may fork children; each takes a fork slot of the Vessel for good" },
];

function AgentPage() {
  useTitle("Agent");
  const { state, agent, live, log } = useFlare();
  const working = agent && agent.state !== "idle";
  return (
    <div className="mx-auto grid max-w-[1400px] gap-8 px-4 py-5 md:px-8 md:py-7 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      {!live && <Sample className="xl:col-span-2" />}
      <div className="min-w-0 space-y-8">
        <Section title="now" right={<Trust verified={false} />}>
          {!agent ? (
            <p className="text-[13px] text-muted-foreground">{live ? "The session has no word of the agent yet." : "The sample session has no agent."}</p>
          ) : (
            <>
              <div className="flex items-center gap-2 text-[13.5px]">
                <Dot tone={working ? "primary" : "muted"} pulse={Boolean(working)} />
                <span className="font-mono text-[12px]">{agent.state}</span>
                <span className="truncate text-muted-foreground">
                  {agent.task ? `task #${agent.task.id}${agent.task.text ? ` · ${agent.task.text}` : ""}` : "no task"} · since {agent.since}
                </span>
              </div>
              {agent.forks.length > 0 && (
                <ul className="mt-2 space-y-1 font-mono text-[11.5px] text-muted-foreground">
                  {agent.forks.map((fork) => <li key={fork.name}>⑂ {fork.name} · {fork.done ? "done" : "working"}</li>)}
                </ul>
              )}
            </>
          )}
          <p className="mt-2 font-mono text-[11px] text-muted-foreground">Told from the agent's requests for tasks: the last state the server saw, not a sign of life.</p>
        </Section>

        <Section title="what it may send">
          <ul className="divide-y divide-border/70">
            {outward.map((p) => (
              <li key={p.kind} className="grid grid-cols-[72px_1fr] gap-x-3 py-1.5">
                <span className="font-mono text-[12px] text-foreground/85">{p.kind}</span>
                <div className="min-w-0">
                  <span className="font-mono text-[12px] text-primary">{p.rule}</span>
                  <p className="text-[12.5px] text-muted-foreground">{p.note}</p>
                </div>
              </li>
            ))}
          </ul>
          <p className="mt-2 font-mono text-[11px] text-muted-foreground">These are this build's rules, not settings: they change with the code, not from this page.</p>
        </Section>
      </div>

      <div className="min-w-0 space-y-8">
        {!state ? <Unread error={log.error} /> : (
          <>
            <Section title="what it was given" right={<Trust verified={false} />}>
              {state.vessels.length === 0 && <p className="text-[13px] text-muted-foreground">No Vessel has an access entry in the log yet.</p>}
              {state.vessels.map((v) => (
                <div key={v.vessel} className="mb-4 last:mb-0">
                  <p className="mb-2 font-mono text-[12px]">Vessel {v.vessel.slice(0, 8)}</p>
                  <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                    {[
                      [`${v.inference_today.calls}`, "model calls today"],
                      [bytes(v.inference_today.bytes_in), "sent today"],
                      [bytes(v.inference_today.bytes_out), "received today"],
                      [`${v.inference.calls}`, "model calls ever"],
                    ].map(([value, label]) => (
                      <div key={label} className="rounded-sm border border-border bg-card p-2.5">
                        <div className="font-mono text-[14px]">{value}</div>
                        <div className="font-mono text-[10.5px] text-muted-foreground">{label}</div>
                      </div>
                    ))}
                  </div>
                  <p className="mt-2 font-mono text-[11px] text-muted-foreground">
                    {Object.entries(v.providers).map(([name, used]) => `${name || "unnamed"} ${used.calls}`).join(" · ") || "no model call yet"}
                    {` · ${count(v.tasks.calls, "task")} pulled · ${count(v.notes.calls, "fork note")} · ${count(v.dispatched, "action")} let out`}
                  </p>
                </div>
              ))}
              <p className="mt-2 font-mono text-[11px] text-muted-foreground">Counted from the log's access entries; "today" is since midnight UTC. The daily budget itself is set on the server, in the island socket's config.</p>
            </Section>

            <Section title="its actions, newest first" right={<Trust verified={false} />}>
              {ledgerOf(state).length === 0 ? <p className="text-[13px] text-muted-foreground">None of its actions has been answered yet.</p> : (
                <ul className="divide-y divide-border/70">
                  {ledgerOf(state).slice(0, 12).map(({ approval, dispatch, outcome }) => (
                    <li key={approval.seq} className="grid grid-cols-[52px_1fr] gap-x-3 py-1.5">
                      <span className="font-mono text-[11.5px] text-muted-foreground">#{approval.seq}</span>
                      <div className="min-w-0">
                        <p className="text-[13px]">
                          <span className={outcome === "sent" ? "text-verified" : outcome === "declined" ? "text-destructive" : "text-unverified"}>{outcome}</span>
                          <span className="ml-2 font-mono text-[12px] text-muted-foreground">req {approval.request.slice(0, 8)}</span>
                        </p>
                        <p className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
                          <span>answered by</span><Key id={approval.by.slice(0, 8)} /><span>{when(approval.at, state.now)}</span>
                          {dispatch && <span>· dispatch #{dispatch.seq}</span>}
                        </p>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </Section>
          </>
        )}
      </div>
    </div>
  );
}
