// Flare's session protocol, version 2, as this page speaks it
// (crates/flare-core/src/session.rs is the definition; docs/slice-2.md,
// "Session protocol, version 2").
//
// The page is one more client of the shared session: it attaches over a
// WebSocket to the server it was loaded from, gets the events, and sends
// commands. It signs nothing: `say` and `see` need no signature, and the
// verbs that do (propose, approve, decline) stay in the terminal until
// this page holds a key of its own.

export type EventKind =
  | "user_message"
  | "agent_text"
  | "action_waiting"
  | "action_settled"
  | "fork_note"
  | "agent_state"
  | "verb_call"
  | "verb_result"
  | "error";

/** One event of the session: its place, when (microseconds), and what. */
export type SessionEvent = {
  seq: number;
  at: number;
  kind: EventKind | (string & {});
  payload: Record<string, unknown>;
};

export type LinkState =
  | { state: "connecting" }
  | { state: "attached"; next: number }
  | { state: "offline"; reason: string };

type ServerMessage =
  | { type: "attached"; session: string; next: number; version: number }
  | ({ type: "event" } & SessionEvent)
  | { type: "refused"; reason: string };

export const SESSION_VERSION = 2;

/** A client's name as the server keeps it: short and plain. */
export function clientName(name: string): string {
  const plain = name
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 20);
  return plain || "page";
}

/** The session's address on the server this page came from. */
export function sessionUrl(location: { protocol: string; host: string }): string {
  return `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/session`;
}

let counter = 0;
/** A command's id: this client's name, and something no other window has. */
export function commandId(client: string): string {
  counter += 1;
  const random = Math.random().toString(36).slice(2, 8);
  return `${client}-${random}-${counter}`.slice(0, 64);
}

export type LinkOptions = {
  url: string;
  session: string;
  client: string;
  onEvent: (event: SessionEvent) => void;
  onLink: (state: LinkState) => void;
  /** For tests: the WebSocket to use. */
  socket?: (url: string) => WebSocket;
};

/**
 * The page's connection to its session. It attaches, hands every event on
 * once and in order, and attaches again from the last event it saw when
 * the connection is lost.
 */
export class SessionLink {
  private socket: WebSocket | null = null;
  private last: number | null = null;
  private attached = false;
  private stopped = false;
  private tries = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly options: LinkOptions) {}

  start(): void {
    this.stopped = false;
    this.open();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.socket?.close();
    this.socket = null;
  }

  /** Say something to the agent. False: not attached, nothing was sent. */
  say(text: string): boolean {
    return this.command("say", { text });
  }

  /** Ask the server what the log shows; the answer is an event of the session. */
  see(stream?: string, entries?: number): boolean {
    return this.command("see", {
      ...(stream === undefined ? {} : { stream }),
      ...(entries === undefined ? {} : { entries }),
    });
  }

  /**
   * Answer an action that waits: an approval (or a decline) this device
   * made and signed, as base64. The command's id, to know its result by;
   * null: not attached, nothing was sent.
   */
  answer(approve: boolean, submission: string, signature: string): string | null {
    return this.send(approve ? "approve" : "decline", { submission, sig_writer: signature });
  }

  private command(verb: "say" | "see", args: Record<string, unknown>): boolean {
    return this.send(verb, args) !== null;
  }

  private send(verb: "say" | "see" | "approve" | "decline", args: Record<string, unknown>): string | null {
    if (!this.attached || this.socket?.readyState !== WebSocket.OPEN) return null;
    const id = commandId(this.options.client);
    this.socket.send(JSON.stringify({ type: "command", id, verb, args }));
    return id;
  }

  private open(): void {
    const { url, session, client, onLink } = this.options;
    onLink({ state: "connecting" });
    let socket: WebSocket;
    try {
      socket = this.options.socket ? this.options.socket(url) : new WebSocket(url);
    } catch (error) {
      this.lost(String(error));
      return;
    }
    this.socket = socket;
    let refused: string | null = null;
    socket.onopen = () => {
      socket.send(
        JSON.stringify({
          type: "attach",
          session,
          client,
          version: SESSION_VERSION,
          ...(this.last === null ? {} : { after: this.last }),
        }),
      );
    };
    socket.onmessage = (message) => {
      let parsed: ServerMessage;
      try {
        parsed = JSON.parse(String(message.data)) as ServerMessage;
      } catch {
        return;
      }
      if (parsed.type === "attached") {
        this.attached = true;
        this.tries = 0;
        onLink({ state: "attached", next: parsed.next });
      } else if (parsed.type === "event") {
        // Each event once, in order: a replay after a lost connection
        // starts after the last one seen.
        if (this.last !== null && parsed.seq <= this.last) return;
        this.last = parsed.seq;
        this.options.onEvent({ seq: parsed.seq, at: parsed.at, kind: parsed.kind, payload: parsed.payload ?? {} });
      } else if (parsed.type === "refused") {
        refused = parsed.reason;
      }
    };
    socket.onclose = () => {
      if (this.socket !== socket) return;
      this.lost(refused ?? "the server cannot be reached");
    };
    socket.onerror = () => {
      // onclose follows and says why.
    };
  }

  private lost(reason: string): void {
    this.attached = false;
    this.socket = null;
    if (this.stopped) return;
    this.options.onLink({ state: "offline", reason });
    const wait = Math.min(15000, 1000 * 2 ** this.tries);
    this.tries += 1;
    this.timer = setTimeout(() => {
      this.timer = null;
      if (!this.stopped) this.open();
    }, wait);
  }
}

/** The log as the server says it is: `GET /log` (crates/flare-server/src/face.rs). */
export type LogLine = { seq: number; at: number; kind: string; stream: string; writer: string; content: string };
export type LogView = {
  head: { seq: number; hash: string } | null;
  entries: number;
  root: string;
  problem: string | null;
  recent: LogLine[];
};

export type LogFilter = { stream?: string; kind?: string; before?: number };

async function read<T>(target: string, signal?: AbortSignal): Promise<T> {
  const answer = await fetch(target, { cache: "no-store", ...(signal ? { signal } : {}) });
  if (!answer.ok) throw new Error(`the server answered ${answer.status}`);
  return (await answer.json()) as T;
}

export function readLog(entries: number, signal?: AbortSignal, only: LogFilter = {}): Promise<LogView> {
  const query = new URLSearchParams({ entries: String(entries) });
  if (only.stream !== undefined) query.set("stream", only.stream);
  if (only.kind !== undefined) query.set("kind", only.kind);
  if (only.before !== undefined) query.set("before", String(only.before));
  return read<LogView>(`/log?${query.toString()}`, signal);
}

/** What the log amounts to: `GET /log/state` (crates/flare-server/src/state.rs). */
export type Counted = { calls: number; bytes_in: number; bytes_out: number };
export type GrantLine = {
  id: string; seq: number; at: number; by: string; holder: string; scope: string[]; rights: string[];
  role: "ordinary" | "root" | "appender"; narrows: string | null; ended_at: number | null; ended_by: string | null;
};
export type PolicyLine = { seq: number; at: number; evaluator: string; invariants: string[] };
export type VerdictLine = { seq: number; at: number; stream: string; writer: string; kind: string; reason: string; content: string };
export type ApprovalLine = { seq: number; at: number; by: string; request: string; verdict: "approve" | "decline"; used_by: number | null };
export type DispatchLine = { seq: number; at: number; by: string; vessel: string; op: number; request: string; approval: number };
export type VesselLine = {
  vessel: string; inference_today: Counted; inference: Counted; providers: Record<string, Counted>;
  reads: Counted; tasks: Counted; notes: Counted; last_access: number | null; last_access_at: number | null; dispatched: number;
};
export type LogState = {
  head: { seq: number; hash: string } | null;
  entries: number;
  root: string;
  appender: string | null;
  state_hash: string;
  problem: string | null;
  now: number;
  kinds: Record<string, number>;
  streams: { name: string; entries: number; last: number; last_at: number }[];
  grants: GrantLine[];
  policy: PolicyLine[];
  refused: number;
  verdicts: VerdictLine[];
  approvals: ApprovalLine[];
  dispatches: DispatchLine[];
  vessels: VesselLine[];
  writers: { key: string; entries: number; last: number; last_at: number; counter: number }[];
};

export function readState(signal?: AbortSignal): Promise<LogState> {
  return read<LogState>("/log/state", signal);
}
