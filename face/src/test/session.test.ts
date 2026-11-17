import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SessionLink, clientName, sessionUrl, type LinkState, type SessionEvent } from "@/lib/session";

/** A WebSocket that records what is sent and is driven by the test. */
class FakeSocket {
  static opened: FakeSocket[] = [];
  readyState = 0;
  sent: Record<string, unknown>[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(readonly url: string) { FakeSocket.opened.push(this); }
  send(text: string) { this.sent.push(JSON.parse(text) as Record<string, unknown>); }
  close() { this.readyState = 3; this.onclose?.(); }
  open() { this.readyState = 1; this.onopen?.(); }
  receive(message: unknown) { this.onmessage?.({ data: JSON.stringify(message) }); }
}

const event = (seq: number) => ({ type: "event", seq, at: 1, kind: "user_message", payload: { id: "x", client: "laptop", text: `m${seq}` } });

describe("the session link", () => {
  let events: SessionEvent[];
  let states: LinkState[];
  let link: SessionLink;
  beforeEach(() => {
    vi.useFakeTimers();
    FakeSocket.opened = [];
    events = [];
    states = [];
    link = new SessionLink({
      url: "ws://10.77.0.1:4749/session", session: "main", client: "phone",
      onEvent: (e) => events.push(e), onLink: (s) => states.push(s),
      socket: (url) => new FakeSocket(url) as unknown as WebSocket,
    });
    link.start();
  });
  afterEach(() => { link.stop(); vi.useRealTimers(); });

  it("attaches without a cursor, then hands on each event once and in order", () => {
    const socket = FakeSocket.opened[0]!;
    socket.open();
    expect(socket.sent).toEqual([{ type: "attach", session: "main", client: "phone", version: 2 }]);
    socket.receive({ type: "attached", session: "main", next: 30, version: 2 });
    // What still stands comes first, with gaps; then the newest.
    for (const seq of [3, 9, 10, 11]) socket.receive(event(seq));
    socket.receive(event(10));
    expect(events.map((e) => e.seq)).toEqual([3, 9, 10, 11]);
    expect(states.at(-1)).toEqual({ state: "attached", next: 30 });
  });

  it("sends nothing before it is attached, and says so", () => {
    const socket = FakeSocket.opened[0]!;
    expect(link.say("hello")).toBe(false);
    socket.open();
    expect(link.say("hello")).toBe(false);
    socket.receive({ type: "attached", session: "main", next: 0, version: 2 });
    expect(link.say("hello")).toBe(true);
    expect(link.see("flare", 5)).toBe(true);
    expect(link.see()).toBe(true);
    const [, say, see, overview] = socket.sent;
    expect(say).toMatchObject({ type: "command", verb: "say", args: { text: "hello" } });
    expect(see).toMatchObject({ type: "command", verb: "see", args: { stream: "flare", entries: 5 } });
    expect(overview).toMatchObject({ verb: "see", args: {} });
    const ids = [say, see, overview].map((command) => String(command?.["id"]));
    expect(new Set(ids).size).toBe(3);
    expect(ids.every((id) => id.startsWith("phone-") && id.length <= 64)).toBe(true);
  });

  it("attaches again after the last event it saw when the connection is lost", () => {
    const first = FakeSocket.opened[0]!;
    first.open();
    first.receive({ type: "attached", session: "main", next: 12, version: 2 });
    first.receive(event(11));
    first.close();
    expect(states.at(-1)).toEqual({ state: "offline", reason: "the server cannot be reached" });
    expect(link.say("lost?")).toBe(false);
    vi.advanceTimersByTime(1000);
    const second = FakeSocket.opened[1]!;
    second.open();
    expect(second.sent[0]).toEqual({ type: "attach", session: "main", client: "phone", version: 2, after: 11 });
    second.receive({ type: "attached", session: "main", next: 14, version: 2 });
    second.receive(event(12));
    expect(events.map((e) => e.seq)).toEqual([11, 12]);
  });

  it("says why the server refused it", () => {
    const socket = FakeSocket.opened[0]!;
    socket.open();
    socket.receive({ type: "refused", reason: "the server is stopping; attach again later" });
    socket.close();
    expect(states.at(-1)).toEqual({ state: "offline", reason: "the server is stopping; attach again later" });
  });

  it("stops for good when it is stopped", () => {
    link.stop();
    vi.advanceTimersByTime(60000);
    expect(FakeSocket.opened).toHaveLength(1);
  });
});

describe("names and addresses", () => {
  it("keeps a client's name short and plain", () => {
    expect(clientName("Jarmo's Phone!")).toBe("jarmo-s-phone");
    expect(clientName("   ")).toBe("page");
    expect(clientName("a".repeat(40))).toHaveLength(20);
  });

  it("finds the session on the server the page came from", () => {
    expect(sessionUrl({ protocol: "http:", host: "10.77.0.1:4749" })).toBe("ws://10.77.0.1:4749/session");
    expect(sessionUrl({ protocol: "https:", host: "flare.example" })).toBe("wss://flare.example/session");
  });
});
