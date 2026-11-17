// What the screens show, worked out from the session's events. Pure
// functions: the same events always give the same screen, on every device.

import type { SessionEvent } from "./session";

export type LineType = "msg" | "reply" | "cmd" | "result" | "fork" | "action" | "settled" | "error" | "state";
/** One line of the session stream. */
export type Line = { id: string; time: string; device: string; type: LineType; text: string };

/** Something that waits for Jarmo. */
export type Waiting =
  | {
      id: string; type: "action"; at: string; text: string; provider: string; hash: string; fork: string;
      /** The stream a verse would go to, when the request is a verse. */
      stream?: string | undefined;
      /** The `action_waiting` event: what `/approve N` names in the terminal. Absent in sample data. */
      event?: number | undefined;
    }
  | { id: string; type: "nudge"; at: string; repo: string; commits: number; since: string }
  | { id: string; type: "verdict"; at: string; stream: string; reason: string; draft: string }
  | { id: string; type: "question"; at: string; text: string };

export type Settled = {
  id: string;
  outcome: "approved" | "declined" | "dismissed" | "override requested" | "drafting" | "sent" | "refused";
  label: string;
  time: string;
  seq?: number | undefined;
};

export type AgentView = {
  state: "idle" | "working" | "forked";
  /** When the server saw it change to this. */
  since: string;
  task?: { id: number; text?: string | undefined } | undefined;
  forks: { name: string; done: boolean; note: string }[];
};

const two = (n: number) => String(n).padStart(2, "0");
/** A time of day, from microseconds since the epoch. */
export function clock(micros: number): string {
  const date = new Date(micros / 1000);
  return `${two(date.getHours())}:${two(date.getMinutes())}`;
}

const text = (value: unknown): string => (typeof value === "string" ? value : "");
const short = (hex: unknown): string => text(hex).slice(0, 8);

/** A fork child's name, short enough for the stream's margin: `fork 11`. */
export function childLabel(child: string): string {
  const slot = /-fork-(\d+)$/.exec(child);
  return slot ? `fork ${slot[1]}` : child;
}

/** A request as it is shown: a verse's stream and words apart, if it is one. */
export function shownVerse(shown: string): { stream: string; text: string } | null {
  const verse = /^a verse on (\S+): ([\s\S]*)$/.exec(shown);
  return verse ? { stream: verse[1] ?? "", text: verse[2] ?? "" } : null;
}

function stateText(payload: Record<string, unknown>): string {
  const task = typeof payload["task"] === "number" ? ` #${payload["task"]}` : "";
  const children = Array.isArray(payload["children"]) ? payload["children"].map((c) => childLabel(String(c))) : [];
  switch (payload["state"]) {
    case "idle": return "the agent is idle";
    case "working": return `the agent is on task${task}`;
    case "forked": return `the agent is on task${task}, forked: ${children.join(", ")}`;
    default: return "the agent's state changed";
  }
}

/** One event as a line of the stream. */
export function line(event: SessionEvent): Line {
  const p = event.payload;
  const base = { id: `e${event.seq}`, time: clock(event.at) };
  switch (event.kind) {
    case "user_message":
      return { ...base, device: text(p["client"]) || "someone", type: "msg", text: text(p["text"]) };
    case "agent_text":
      return { ...base, device: "agent", type: "reply", text: text(p["text"]) };
    case "action_waiting":
      return { ...base, device: "agent", type: "action", text: `waiting for you: ${text(p["shown"])} (req ${short(p["request_hash"])}) · #${event.seq}` };
    case "action_settled": {
      const entry = typeof p["entry"] === "number" ? ` · entry #${p["entry"]}` : "";
      const why = text(p["why"]) ? ` · ${text(p["why"])}` : "";
      return { ...base, device: "server", type: p["settled"] === "sent" ? "settled" : "result", text: `${text(p["settled"])}: req ${short(p["request_hash"])}${entry}${why}` };
    }
    case "fork_note":
      return { ...base, device: childLabel(text(p["child"])), type: "fork", text: `${p["done"] === true ? "done: " : ""}${text(p["text"])}` };
    case "agent_state":
      return { ...base, device: "server", type: "state", text: stateText(p) };
    case "verb_call":
      return { ...base, device: text(p["client"]) || "someone", type: "cmd", text: text(p["what"]) };
    case "verb_result": {
      const at = p["at"] as { seq?: unknown } | null | undefined;
      const place = at && typeof at.seq === "number" ? `\n(at entry #${at.seq})` : "";
      return { ...base, device: "server", type: p["ok"] === false ? "error" : "result", text: `${text(p["text"]).trimEnd()}${place}` };
    }
    case "error":
      return { ...base, device: "server", type: "error", text: text(p["text"]) };
    default:
      return { ...base, device: "server", type: "result", text: `an event this page does not read: ${event.kind}` };
  }
}

/** The actions still waiting for an answer, oldest first. */
export function waitingOf(events: readonly SessionEvent[]): Waiting[] {
  const settled = new Set(events.filter((e) => e.kind === "action_settled").map((e) => text(e.payload["request_hash"])));
  return events
    .filter((e) => e.kind === "action_waiting" && !settled.has(text(e.payload["request_hash"])))
    .map((e) => {
      const shown = text(e.payload["shown"]);
      const verse = shownVerse(shown);
      return {
        id: `#${e.seq}`,
        type: "action" as const,
        at: clock(e.at),
        text: verse ? verse.text : shown,
        provider: text(e.payload["provider"]),
        hash: short(e.payload["request_hash"]),
        fork: `Vessel ${short(e.payload["vessel"])}`,
        stream: verse?.stream,
        event: e.seq,
      };
    });
}

/** What was answered, newest first. */
export function settledOf(events: readonly SessionEvent[]): Settled[] {
  const shown = new Map(events.filter((e) => e.kind === "action_waiting").map((e) => [text(e.payload["request_hash"]), text(e.payload["shown"])]));
  return events
    .filter((e) => e.kind === "action_settled")
    .map((e) => {
      const hash = text(e.payload["request_hash"]);
      const outcome = text(e.payload["settled"]);
      return {
        id: `e${e.seq}`,
        outcome: (outcome === "sent" || outcome === "declined" || outcome === "refused" ? outcome : "refused") as Settled["outcome"],
        label: shown.get(hash) ?? `req ${hash.slice(0, 8)}`,
        time: clock(e.at),
        seq: typeof e.payload["entry"] === "number" ? e.payload["entry"] : undefined,
      };
    })
    .reverse();
}

/** What the agent is doing, as the server last said, with its children's newest notes. */
export function agentOf(events: readonly SessionEvent[]): AgentView | null {
  let index = -1;
  for (let i = events.length - 1; i >= 0; i--) {
    if (events[i]?.kind === "agent_state") { index = i; break; }
  }
  const event = events[index];
  if (!event) return null;
  const p = event.payload;
  const state = p["state"] === "working" || p["state"] === "forked" ? p["state"] : "idle";
  const id = typeof p["task"] === "number" ? p["task"] : undefined;
  const said = id === undefined ? undefined : events.find((e) => e.seq === id && e.kind === "user_message");
  const children = Array.isArray(p["children"]) ? p["children"].map(String) : [];
  const forks = children.map((name) => {
    let note: SessionEvent | undefined;
    for (const e of events) {
      if (e.kind === "fork_note" && e.payload["child"] === name && (id === undefined || e.seq > id)) note = e;
    }
    return { name, done: note?.payload["done"] === true, note: text(note?.payload["text"]) };
  });
  return {
    state,
    since: clock(event.at),
    task: id === undefined ? undefined : { id, text: said ? text(said.payload["text"]) : undefined },
    forks,
  };
}
