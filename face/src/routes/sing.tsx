import { createFileRoute } from "@tanstack/react-router";
import { useTitle } from "@/lib/title";
import { useState } from "react";
import { Dot, Key, Sample, Section, Trust } from "@/components/flare/marks";
import { Button } from "@/components/ui/button";
import { Flame } from "lucide-react";
import { VERSE_MAX_BYTES, drafts, evidenceKinds, singableStreams, sungRecently, type Draft } from "@/lib/sing-mock";

export const Route = createFileRoute("/sing")({
  component: SingPage,
});

const byteCount = (s: string) => new TextEncoder().encode(s).length;

function SingPage() {
  useTitle("Sing");
  const [stream, setStream] = useState(singableStreams[0]?.stream ?? "taso");
  const [evidence, setEvidence] = useState<(typeof evidenceKinds)[number]["kind"]>("decision");
  const [text, setText] = useState("");
  const bytes = byteCount(text);
  const over = bytes > VERSE_MAX_BYTES;

  return (
    <div className="mx-auto grid max-w-[1400px] gap-8 px-4 py-5 md:px-8 md:py-7 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <Sample what="Sing" className="xl:col-span-2" />
      <div className="min-w-0 space-y-8">
        <Section title="compose a verse" right={<span className={over ? "font-mono text-[11px] text-destructive" : "font-mono text-[11px] text-muted-foreground"}>{bytes}/{VERSE_MAX_BYTES} bytes</span>}>
          <div className="flex flex-wrap gap-1.5" role="group" aria-label="stream">
            {singableStreams.map((s) => (
              <button key={s.stream} onClick={() => setStream(s.stream)} title={s.note}
                className={s.stream === stream
                  ? "rounded-sm border border-gold bg-gold/15 px-2 py-1 font-mono text-[11.5px] text-foreground"
                  : "rounded-sm border border-border px-2 py-1 font-mono text-[11.5px] text-muted-foreground hover:text-foreground"}>
                {s.stream}
              </button>
            ))}
          </div>
          <div className="mt-2 flex flex-wrap gap-1.5" role="group" aria-label="evidence">
            {evidenceKinds.map((e) => (
              <button key={e.kind} onClick={() => setEvidence(e.kind)} title={e.hint}
                className={e.kind === evidence
                  ? "rounded-sm border border-gold bg-gold/15 px-2 py-1 font-mono text-[11.5px] text-foreground"
                  : "rounded-sm border border-border px-2 py-1 font-mono text-[11.5px] text-muted-foreground hover:text-foreground"}>
                {e.kind}
              </button>
            ))}
          </div>
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={5}
            placeholder="Sing what you decided, built, measured or closed…"
            className="mt-3 w-full resize-y rounded-sm border border-border bg-card p-2.5 font-mono text-[13px] outline-none placeholder:text-muted-foreground/50 focus:border-gold/70"
          />
          <div className="mt-2 flex items-center gap-2">
            <Button size="sm" disabled={over || bytes === 0} className="gap-1.5">
              <Flame className="h-3.5 w-3.5" /> Sing into {stream}
            </Button>
            <span className="font-mono text-[11px] text-muted-foreground">
              {over ? "the gate will refuse this (R2)" : evidence === "artefact" ? "artefact verses need a ref with a receipt (R3, R4)" : "gate checks R1–R6 on the way in"}
            </span>
          </div>
        </Section>

        <Section title={`drafts · ${drafts.length}`}>
          <ul className="divide-y divide-border/70">
            {drafts.map((d: Draft) => (
              <li key={d.id} className="py-2">
                <div className="flex items-center gap-2 font-mono text-[11px] text-muted-foreground">
                  <span className="text-foreground/85">{d.stream}</span>
                  <span>{d.evidence}</span>
                  <span className="ml-auto">{d.bytes} B · {d.updated}</span>
                </div>
                <p className="mt-1 text-[13px]">{d.text}</p>
              </li>
            ))}
          </ul>
        </Section>
      </div>

      <div className="min-w-0 space-y-8">
        <Section title="sung recently">
          <ul className="divide-y divide-border/70">
            {sungRecently.map((v) => (
              <li key={v.seq} className="grid grid-cols-[44px_1fr] gap-x-3 py-1.5">
                <span className="font-mono text-[11.5px] text-muted-foreground">#{v.seq}</span>
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
                    <span className="text-foreground/85">{v.stream}</span>
                    <span>{v.evidence}</span>
                    <Key id={v.writer.id} label={v.writer.label} />
                    <span className="ml-auto flex items-center gap-2">{v.time}<Trust verified={v.verified} /></span>
                  </div>
                  <p className="truncate text-[13px]">{v.text}</p>
                </div>
              </li>
            ))}
          </ul>
        </Section>

        <Section title="the rules of the gate">
          <ul className="space-y-1.5 font-mono text-[11.5px] text-muted-foreground">
            <li className="flex items-center gap-1.5"><Dot tone="verified" /> R1 · verses name their evidence</li>
            <li className="flex items-center gap-1.5"><Dot tone="verified" /> R2 · verse text at most {VERSE_MAX_BYTES} bytes</li>
            <li className="flex items-center gap-1.5"><Dot tone="verified" /> R3 · artefact verses carry a ref</li>
            <li className="flex items-center gap-1.5"><Dot tone="verified" /> R4 · artefact refs have a receipt</li>
          </ul>
        </Section>
      </div>
    </div>
  );
}
