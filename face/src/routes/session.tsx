import { createFileRoute } from "@tanstack/react-router";
import { SessionStream } from "@/components/flare/Session";
import { WaitingList } from "@/components/flare/Waiting";
import { Section } from "@/components/flare/marks";
import { useFlare } from "@/components/flare/store";
import { useTitle } from "@/lib/title";

export const Route = createFileRoute("/session")({ component: SessionPage });

function SessionPage() {
  useTitle("Session");
  const { waiting, live, session, client, agent } = useFlare();
  const doing = !live || !agent ? null : agent.state === "idle" ? "agent idle" : `agent on task #${agent.task?.id ?? "?"}${agent.state === "forked" ? `, forked ${agent.forks.length}` : ""}`;
  return (
    <div className="mx-auto grid h-[calc(100dvh-10.5rem)] max-w-[1400px] gap-8 px-3 py-4 md:h-screen md:px-8 md:py-6 lg:grid-cols-[minmax(0,1fr)_360px]">
      <Section title={live ? `session ${session} · you are ${client}` : "session · laptop, phone, terminal"} right={doing && <span className="font-mono text-[11px] text-muted-foreground">{doing}</span>}
        className="flex min-h-0 flex-col [&>div:last-child]:min-h-0 [&>div:last-child]:flex-1">
        <div><SessionStream /></div>
      </Section>
      <Section title={`waiting for you · ${waiting.length}`} className="hidden overflow-y-auto lg:block"><WaitingList /></Section>
    </div>
  );
}
