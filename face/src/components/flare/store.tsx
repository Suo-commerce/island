import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { initialSession, initialWaiting, logHead } from "@/lib/mock";
import { sampleState } from "@/lib/sample-state";
import { SessionLink, clientName, readLog, readState, sessionUrl, type LinkState, type LogState, type LogView, type SessionEvent } from "@/lib/session";
import { agentOf, line, settledOf, waitingOf, type AgentView, type Line, type Settled, type Waiting } from "@/lib/view";

export type { Settled } from "@/lib/view";

/** What the page knows of the log, and when it last asked. */
export type LogRead = { view: LogView | null; readAt: string | null; error: string | null };

type Ctx = {
  /** Live: on the real session. Otherwise the prototype's sample data. */
  live: boolean;
  link: LinkState;
  /** The session's name and this page's name in it. */
  session: string;
  client: string;
  rename: (name: string) => void;
  waiting: Waiting[];
  lines: Line[];
  settled: Settled[];
  /** Live only: what the server last said the agent is doing. */
  agent: AgentView | null;
  /** Live only. */
  log: LogRead;
  /** What the log amounts to: streams, grants, policy, Vessels. Sample data when not live. */
  state: LogState | null;
  /** The newest entry's place, as far as the page was told. */
  head: number | null;
  /** Say `text` to the agent, or run a `/see`. A string: why nothing was sent. */
  send: (text: string) => string | null;
  /** Sample data only: answer something that waits. */
  settle: (id: string, outcome: Settled["outcome"], label: string, event: Omit<Line, "id" | "time">) => number | undefined;
};

const C = createContext<Ctx | null>(null);

/** The page runs on sample data when it is built or started for that. */
export const SAMPLE = import.meta.env["VITE_FLARE_MOCK"] === "1";
const SESSION = "main";
const CLIENT_KEY = "flare.client";
/** How many events the page keeps; older ones leave the stream. */
const KEEP = 2000;
const LOG_ENTRIES = 14;

function storedClient(): string {
  try {
    const stored = window.localStorage.getItem(CLIENT_KEY);
    if (stored) return clientName(stored);
  } catch {
    // No storage: the default name.
  }
  return window.innerWidth < 768 ? "phone" : "browser";
}

/** What the page cannot do yet, said instead of sent. */
function notHere(command: string): string {
  switch (command) {
    case "approve":
    case "decline":
      return `/${command} is signed with a device's key, and this page holds none yet. Answer it in the terminal.`;
    case "verse":
    case "again":
      return `/${command} is signed with a device's key, and this page holds none yet. Sing it in the terminal.`;
    case "waiting":
      return "What waits is listed on Now.";
    default:
      return `This page does not know /${command}. It takes /see [STREAM [N]] and /say TEXT; anything else typed is said to the agent.`;
  }
}

function LiveProvider({ children }: { children: ReactNode }) {
  const [events, setEvents] = useState<SessionEvent[]>([]);
  const [link, setLink] = useState<LinkState>({ state: "connecting" });
  const [client, setClient] = useState(storedClient);
  const [log, setLog] = useState<LogRead>({ view: null, readAt: null, error: null });
  const [state, setState] = useState<LogState | null>(null);
  const linkRef = useRef<SessionLink | null>(null);

  useEffect(() => {
    setEvents([]);
    const session = new SessionLink({
      url: sessionUrl(window.location),
      session: SESSION,
      client,
      onEvent: (event) => setEvents((seen) => (seen.length >= KEEP ? [...seen.slice(seen.length - KEEP + 1), event] : [...seen, event])),
      onLink: setLink,
    });
    linkRef.current = session;
    session.start();
    return () => {
      session.stop();
      linkRef.current = null;
    };
  }, [client]);

  // The log, read when the page attaches, every half minute while it is
  // looked at, and shortly after something that may have written to it.
  const attached = link.state === "attached";
  const moved = useMemo(() => {
    for (let i = events.length - 1; i >= 0; i--) {
      const kind = events[i]?.kind;
      if (kind === "action_settled" || kind === "verb_result") return events[i]?.seq ?? 0;
    }
    return 0;
  }, [events]);
  useEffect(() => {
    if (!attached) return;
    const abort = new AbortController();
    const read = () => {
      if (document.visibilityState === "hidden") return;
      readState(abort.signal).then(setState).catch(() => {
        // The log's own read says what is wrong.
      });
      readLog(LOG_ENTRIES, abort.signal)
        .then((view) => setLog({ view, readAt: new Date().toTimeString().slice(0, 8), error: null }))
        .catch((error: unknown) => {
          if (!abort.signal.aborted) setLog((before) => ({ ...before, error: String(error instanceof Error ? error.message : error) }));
        });
    };
    const soon = setTimeout(read, moved === 0 ? 0 : 1200);
    const again = setInterval(read, 30000);
    document.addEventListener("visibilitychange", read);
    return () => {
      abort.abort();
      clearTimeout(soon);
      clearInterval(again);
      document.removeEventListener("visibilitychange", read);
    };
  }, [attached, moved]);

  const send = useCallback<Ctx["send"]>((typed) => {
    const session = linkRef.current;
    const trimmed = typed.trim();
    if (!trimmed) return null;
    const unsent = "Not attached to the session: nothing was sent.";
    if (!trimmed.startsWith("/")) return session?.say(trimmed) ? null : unsent;
    const [command = "", ...words] = trimmed.slice(1).split(/\s+/);
    if (command === "say") {
      const said = trimmed.slice(4).trim();
      if (!said) return "/say needs something to say.";
      return session?.say(said) ? null : unsent;
    }
    if (command === "see") {
      const count = words[1] === undefined ? undefined : Number(words[1]);
      if (count !== undefined && !Number.isInteger(count)) return `\`${words[1]}\` is not a number.`;
      return session?.see(words[0], count) ? null : unsent;
    }
    return notHere(command);
  }, []);

  const rename = useCallback((name: string) => {
    const plain = clientName(name);
    try {
      window.localStorage.setItem(CLIENT_KEY, plain);
    } catch {
      // Kept for this visit only.
    }
    setClient(plain);
  }, []);

  const value = useMemo<Ctx>(() => ({
    live: true,
    link,
    session: SESSION,
    client,
    rename,
    waiting: waitingOf(events),
    // The agent's state has its own place on the screens; in the stream
    // it would be two more lines around every message.
    lines: events.filter((event) => event.kind !== "agent_state").map(line),
    settled: settledOf(events),
    agent: agentOf(events),
    log,
    state,
    head: log.view?.head?.seq ?? null,
    send,
    settle: () => undefined,
  }), [events, link, client, rename, log, state, send]);
  return <C.Provider value={value}>{children}</C.Provider>;
}

const now = () => new Date().toTimeString().slice(0, 5);

/** The prototype, as it was: sample data, and answers that go nowhere. */
function SampleProvider({ children }: { children: ReactNode }) {
  const [waiting, setWaiting] = useState(initialWaiting);
  const [lines, setLines] = useState(initialSession);
  const [settled, setSettled] = useState<Settled[]>([]);
  const [nextSeq, setNextSeq] = useState(logHead.seq + 1);
  const add = (e: Omit<Line, "id" | "time">) => setLines((s) => [...s, { ...e, id: "s" + s.length + Math.random(), time: now() }]);
  const send: Ctx["send"] = (typed) => {
    const t = typed.trim();
    if (!t) return null;
    const command = t.startsWith("/");
    add({ device: window.innerWidth < 768 ? "phone" : "laptop", type: command ? "cmd" : "msg", text: t });
    setTimeout(() => add({ device: command ? "server" : "agent", type: command ? "result" : "reply", text: command ? "ok" : "Noted. Continuing on taso-deadlines." }), 700);
    return null;
  };
  const settle: Ctx["settle"] = (id, outcome, label, event) => {
    const seq = outcome === "approved" ? nextSeq : undefined;
    if (seq) setNextSeq(seq + 1);
    setWaiting((w) => w.filter((x) => x.id !== id));
    setSettled((s) => [{ id, outcome, label, time: now(), seq }, ...s]);
    add({ ...event, text: event.text + (seq ? ` · sent once · #${seq}` : "") });
    return seq;
  };
  const value: Ctx = {
    live: false,
    link: { state: "attached", next: 0 },
    session: SESSION,
    client: "laptop",
    rename: () => {},
    waiting, lines, settled,
    agent: null,
    log: { view: null, readAt: null, error: null },
    state: sampleState,
    head: nextSeq - 1,
    send, settle,
  };
  return <C.Provider value={value}>{children}</C.Provider>;
}

export function FlareProvider({ children, sample = SAMPLE }: { children: ReactNode; sample?: boolean }) {
  return sample ? <SampleProvider>{children}</SampleProvider> : <LiveProvider>{children}</LiveProvider>;
}

export const useFlare = () => {
  const c = useContext(C);
  if (!c) throw new Error("FlareProvider missing");
  return c;
};
