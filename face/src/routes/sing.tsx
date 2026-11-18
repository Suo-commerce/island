import { createFileRoute } from "@tanstack/react-router";
import { Copy } from "lucide-react";
import { useEffect, useState } from "react";

import { Dot, Key, Sample, Section, Trust } from "@/components/flare/marks";
import { useFlare } from "@/components/flare/store";
import { Button } from "@/components/ui/button";
import { inForce, singable, verseMax, when } from "@/lib/island";
import { readLog, type LogLine } from "@/lib/session";
import { copyText, verseCommand } from "@/lib/sing";
import { useTitle } from "@/lib/title";

export const Route = createFileRoute("/sing")({
  component: SingPage,
});

const evidenceKinds = [
  { kind: "decision", hint: "a choice made, with the reason" },
  { kind: "artefact", hint: "something built: needs ref=HASH, the hash of what was built" },
  { kind: "measurement", hint: "a number you measured, with how" },
  { kind: "closed-question", hint: "a question asked and answered, closed" },
] as const;
type Evidence = (typeof evidenceKinds)[number]["kind"];

const chip = (on: boolean) => on
  ? "rounded-sm border border-gold bg-gold/15 px-2 py-1 font-mono text-[11.5px] text-foreground"
  : "rounded-sm border border-border px-2 py-1 font-mono text-[11.5px] text-muted-foreground hover:text-foreground";

const DRAFT = "flare.sing.draft";
type Draft = { stream: string; evidence: Evidence; text: string };

function stored(): Draft | null {
  try {
    const draft = JSON.parse(window.localStorage.getItem(DRAFT) ?? "null") as Draft | null;
    return draft && typeof draft.text === "string" ? draft : null;
  } catch {
    return null;
  }
}

function SingPage() {
  useTitle("Sing");
  const { state, live, head } = useFlare();
  const streams = singable(state);
  const max = verseMax(state);
  const [draft, setDraft] = useState<Draft>(() => stored() ?? { stream: "", evidence: "decision", text: "" });
  const [copied, setCopied] = useState<string | null>(null);
  const stream = streams.some((s) => s.name === draft.stream) ? draft.stream : (streams[0]?.name ?? "");
  const size = new TextEncoder().encode(draft.text.trim()).length;
  const over = max !== null && size > max;
  const command = verseCommand(stream, draft.evidence, draft.text);

  // The draft is kept in this browser, and nowhere else.
  useEffect(() => {
    try {
      window.localStorage.setItem(DRAFT, JSON.stringify(draft));
    } catch {
      // Kept for this visit only.
    }
  }, [draft]);

  const copy = () => {
    if (!command) return;
    void copyText(command).then((done) =>
      setCopied(done ? "Copied. Paste it into the Flare terminal." : "This browser would not copy it: select the command and copy it by hand."));
  };

  return (
    <div className="mx-auto grid max-w-[1400px] gap-8 px-4 py-5 md:px-8 md:py-7 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      {!live && <Sample className="xl:col-span-2" />}
      <div className="min-w-0 space-y-8">
        <Section title="compose a verse" right={<span className={over ? "font-mono text-[11px] text-destructive" : "font-mono text-[11px] text-muted-foreground"}>{size}{max === null ? "" : `/${max}`} bytes</span>}>
          <div className="flex flex-wrap gap-1.5" role="group" aria-label="stream">
            {streams.length === 0 && <span className="font-mono text-[11.5px] text-muted-foreground">{state ? "no stream to sing on yet" : "reading the streams…"}</span>}
            {streams.map((s) => (
              <button key={s.name} onClick={() => setDraft({ ...draft, stream: s.name })} title={`${s.entries} ${s.entries === 1 ? "entry" : "entries"}`} className={chip(s.name === stream)}>{s.name}</button>
            ))}
          </div>
          <div className="mt-2 flex flex-wrap gap-1.5" role="group" aria-label="evidence">
            {evidenceKinds.map((e) => (
              <button key={e.kind} onClick={() => setDraft({ ...draft, evidence: e.kind })} title={e.hint} className={chip(e.kind === draft.evidence)}>{e.kind}</button>
            ))}
          </div>
          <textarea
            value={draft.text}
            onChange={(e) => { setDraft({ ...draft, text: e.target.value }); setCopied(null); }}
            rows={5}
            aria-label="The verse"
            placeholder="Sing what you decided, built, measured or closed…"
            className="mt-3 w-full resize-y rounded-sm border border-border bg-card p-2.5 font-mono text-[13px] outline-none placeholder:text-muted-foreground/50 focus:border-gold/70"
          />
          <p className="mt-1 font-mono text-[11px] text-muted-foreground">
            {over ? `The gate will refuse this: a verse holds at most ${max} bytes.`
              : draft.evidence === "artefact" ? "An artefact verse must point at what was built: add ref=HASH before the text in the terminal."
              : evidenceKinds.find((e) => e.kind === draft.evidence)?.hint}
          </p>
          <div className="mt-3 rounded-sm border border-border bg-card px-3 py-2">
            <p className="text-[12.5px] text-muted-foreground">
              A verse is signed with a device's key, and this page holds none yet. Sing it in the Flare terminal with this command:
            </p>
            <p className="mt-1.5 select-all whitespace-pre-wrap break-words font-mono text-[12.5px]">{command ?? "/verse @STREAM EVIDENCE TEXT"}</p>
            <div className="mt-2 flex flex-wrap items-center gap-2">
              <Button size="sm" variant="outline" disabled={!command || over} onClick={copy} className="gap-1.5"><Copy className="h-3.5 w-3.5" /> Copy the command</Button>
              {copied && <span role="status" className="font-mono text-[11px] text-muted-foreground">{copied}</span>}
            </div>
          </div>
          <p className="mt-2 font-mono text-[11px] text-muted-foreground">The draft is kept in this browser only.</p>
        </Section>

        <Section title="the rules of the gate" right={<Trust verified={false} />}>
          {!state ? <p className="text-[13px] text-muted-foreground">Reading the policy…</p>
            : inForce(state.policy).length === 0 ? <p className="text-[13px] text-muted-foreground">No policy entry yet: nothing is required of a verse.</p> : (
              <ul className="space-y-1.5 font-mono text-[11.5px] text-muted-foreground">
                {inForce(state.policy).map((rule) => (
                  <li key={rule.invariant} className="flex items-center gap-1.5"><Dot tone="verified" /> {rule.invariant} <span className="ml-auto">since #{rule.since}</span></li>
                ))}
              </ul>
            )}
        </Section>
      </div>

      <div className="min-w-0 space-y-8">
        <Sung live={live} head={head} />
      </div>
    </div>
  );
}

/** The newest verses of the log. */
function Sung({ live, head }: { live: boolean; head: number | null }) {
  const [verses, setVerses] = useState<LogLine[] | null>(live ? null : []);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!live) return;
    const abort = new AbortController();
    readLog(30, abort.signal, { kind: "verse" })
      .then((log) => { setVerses(log.recent); setError(null); })
      .catch((problem: unknown) => { if (!abort.signal.aborted) setError(problem instanceof Error ? problem.message : String(problem)); });
    return () => abort.abort();
  }, [live, head]);
  return (
    <Section title="sung recently">
      {verses === null && <p className="py-6 text-center text-[13px] text-muted-foreground">{error ? `The verses could not be read: ${error}.` : "Reading the verses…"}</p>}
      {verses?.length === 0 && <p className="py-6 text-center text-[13px] text-muted-foreground">{live ? "No verse has been sung yet." : "The sample has no verses."}</p>}
      <ul className="divide-y divide-border/70">
        {(verses ?? []).map((v) => (
          <li key={v.seq} className="grid grid-cols-[44px_1fr] gap-x-3 py-1.5">
            <span className="font-mono text-[11.5px] text-muted-foreground">#{v.seq}</span>
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
                <span className="text-foreground/85">{v.stream}</span>
                <Key id={v.writer.slice(0, 8)} />
                <span className="ml-auto flex items-center gap-2">{when(v.at)}<Trust verified={false} /></span>
              </div>
              <p className="whitespace-pre-wrap break-words text-[13px]">{v.content}</p>
            </div>
          </li>
        ))}
      </ul>
    </Section>
  );
}
