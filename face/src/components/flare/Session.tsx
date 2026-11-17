import { useEffect, useRef, useState } from "react";
import { useFlare } from "./store";
import { cn } from "@/lib/utils";

const devTone: Record<string, string> = { agent: "text-primary", server: "text-muted-foreground" };

export function SessionStream({ compact }: { compact?: boolean }) {
  const { lines, send, link, live } = useFlare();
  const [text, setText] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => { end.current?.scrollIntoView({ block: "end" }); }, [lines.length]);

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!text.trim()) return;
    const unsent = send(text);
    setNotice(unsent);
    if (unsent === null) setText("");
  };
  const attached = link.state === "attached";

  return (
    <div className="flex h-full min-h-0 flex-col">
      <ol className={cn("flex-1 space-y-px overflow-y-auto border-l-[3px] border-gold/70 pl-2 font-mono text-[12.5px]", compact && "max-h-[60vh]")}>
        {lines.length === 0 && (
          <li className="px-1.5 py-1 text-muted-foreground">
            {attached ? "Nothing has happened in this session yet." : link.state === "connecting" ? "Attaching to the session…" : `Not attached: ${link.reason}.`}
          </li>
        )}
        {lines.map((e) => (
          <li key={e.id} className={cn("grid grid-cols-[38px_58px_minmax(0,1fr)] gap-2 rounded-sm px-1.5 py-1 leading-snug",
            e.type === "action" && "bg-primary/10", e.type === "error" && "bg-destructive/10")}>
            <span className="text-muted-foreground/70">{e.time}</span>
            <span className={cn("truncate", devTone[e.device] ?? "text-foreground/80")} title={e.device}>{e.device}</span>
            <span className={cn("whitespace-pre-wrap break-words",
              (e.type === "msg" || e.type === "reply") && "font-sans text-[13.5px]",
              e.type === "cmd" && "text-foreground",
              (e.type === "result" || e.type === "state") && "text-muted-foreground",
              e.type === "fork" && "text-muted-foreground italic",
              e.type === "settled" && "text-verified",
              e.type === "error" && "text-destructive",
              e.type === "action" && "text-primary",
            )}>
              {e.type === "cmd" && <span className="text-muted-foreground">$ </span>}
              {e.type === "fork" && "⑂ "}
              {e.text}
            </span>
          </li>
        ))}
        <div ref={end} />
      </ol>
      {notice && <p role="status" className="border-t border-border pt-2 font-mono text-[11.5px] text-unverified">{notice}</p>}
      <form onSubmit={submit} className="flex gap-2 border-t border-border pt-2">
        <input value={text} onChange={(e) => { setText(e.target.value); setNotice(null); }}
          aria-label="Message the agent" placeholder={live ? "message the agent, or /see" : "message the agent, or /command"}
          className="h-10 min-w-0 flex-1 rounded-sm border border-input bg-card px-3 text-[13.5px] placeholder:text-muted-foreground/70 focus:outline-none focus:ring-1 focus:ring-ring" />
        <button disabled={!attached} className="h-10 rounded-sm bg-primary px-4 text-[13px] font-medium text-primary-foreground disabled:opacity-50">Send</button>
      </form>
    </div>
  );
}
