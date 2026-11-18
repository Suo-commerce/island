import { describe, expect, it } from "vitest";

import type { SessionEvent } from "@/lib/session";
import { agentOf, childLabel, line, settledOf, shownVerse, waitingOf } from "@/lib/view";

const at = 1_794_400_000_000_000;
const event = (seq: number, kind: string, payload: Record<string, unknown>): SessionEvent => ({ seq, at: at + seq * 1_000_000, kind, payload });
const waits = (seq: number, hash: string, shown: string) =>
  event(seq, "action_waiting", { vessel: "914c503c980b8f7f1750c664b5743d2c", op: 7, provider: "island.verse", request_hash: hash, request: "00", shown });

describe("what waits", () => {
  const a = "aa".repeat(32);
  const b = "bb".repeat(32);
  const events = [
    waits(3, a, 'a verse on flare: [decision] "Keep it"'),
    waits(5, b, "island.mail, 120 bytes"),
    event(8, "action_settled", { request_hash: a, settled: "sent", entry: 75, why: null }),
  ];

  it("is every action without an answer, named by its event", () => {
    const waiting = waitingOf(events);
    expect(waiting).toHaveLength(1);
    expect(waiting[0]).toMatchObject({ id: "#5", type: "action", event: 5, request: b, hash: "bbbbbbbb", provider: "island.verse", text: "island.mail, 120 bytes", fork: "Vessel 914c503c" });
    expect(waiting[0] && "stream" in waiting[0] ? waiting[0].stream : "none").toBeUndefined();
  });

  it("shows a verse's stream and words apart", () => {
    expect(waitingOf(events.slice(0, 1))[0]).toMatchObject({ stream: "flare", text: '[decision] "Keep it"', event: 3 });
    expect(shownVerse("island.mail, 120 bytes")).toBeNull();
  });

  it("lists what was answered with the entry it became", () => {
    expect(settledOf(events)).toEqual([expect.objectContaining({ outcome: "sent", seq: 75, label: 'a verse on flare: [decision] "Keep it"' })]);
  });
});

describe("the agent's state", () => {
  it("is unknown until the server says one", () => {
    expect(agentOf([event(1, "user_message", { id: "x", client: "laptop", text: "hello" })])).toBeNull();
  });

  it("is the newest one, with the task's words and each child's newest note", () => {
    const events = [
      event(0, "agent_state", { state: "idle" }),
      event(1, "fork_note", { child: "flare-home-fork-11", text: "from an older task", done: true }),
      event(4, "user_message", { id: "x", client: "laptop", text: "two ways" }),
      event(5, "agent_state", { state: "working", task: 4 }),
      event(6, "fork_note", { child: "flare-home-fork-11", text: "started", done: false }),
      event(7, "agent_state", { state: "forked", task: 4, children: ["flare-home-fork-11"] }),
      event(8, "fork_note", { child: "flare-home-fork-12", text: "started", done: false }),
      event(9, "agent_state", { state: "forked", task: 4, children: ["flare-home-fork-11", "flare-home-fork-12"] }),
      event(10, "fork_note", { child: "flare-home-fork-11", text: "a draft, in draft.md", done: true }),
    ];
    expect(agentOf(events)).toMatchObject({
      state: "forked",
      task: { id: 4, text: "two ways" },
      forks: [
        { name: "flare-home-fork-11", done: true, note: "a draft, in draft.md" },
        { name: "flare-home-fork-12", done: false, note: "started" },
      ],
    });
    expect(agentOf(events.slice(0, 1))).toMatchObject({ state: "idle", forks: [] });
    expect(agentOf(events.slice(0, 1))?.task).toBeUndefined();
  });

  it("names a task it was not shown by its number alone", () => {
    expect(agentOf([event(40, "agent_state", { state: "working", task: 31 })])?.task).toEqual({ id: 31, text: undefined });
  });
});

describe("a line of the stream", () => {
  it("says who and what for every kind of event", () => {
    expect(line(event(1, "user_message", { id: "x", client: "laptop", text: "hello" }))).toMatchObject({ device: "laptop", type: "msg", text: "hello" });
    expect(line(event(2, "agent_text", { task: 1, text: "Hi.\nTwo lines.", vessel: "v", key: "k" }))).toMatchObject({ device: "agent", type: "reply", text: "Hi.\nTwo lines." });
    expect(line(event(3, "fork_note", { child: "flare-home-fork-11", text: "made it", done: true }))).toMatchObject({ device: "fork 11", type: "fork", text: "done: made it" });
    expect(line(event(4, "agent_state", { state: "forked", task: 1, children: ["flare-home-fork-11"] })).text).toBe("the agent is on task #1, forked: fork 11");
    expect(line(waits(5, "cd".repeat(32), "a verse on flare: x")).text).toBe("waiting for you: a verse on flare: x (req cdcdcdcd) · #5");
    expect(line(event(6, "action_settled", { request_hash: "cd".repeat(32), settled: "declined", entry: null, why: "not now" }))).toMatchObject({ type: "result", text: "declined: req cdcdcdcd · not now" });
    expect(line(event(7, "verb_call", { id: "c", client: "phone", verb: "see", what: "see flare" }))).toMatchObject({ device: "phone", type: "cmd", text: "see flare" });
    expect(line(event(8, "verb_result", { id: "c", ok: true, text: "the log\n", at: { seq: 108, hash: "ab" } })).text).toBe("the log\n(at entry #108)");
    expect(line(event(9, "verb_result", { id: "c", ok: false, text: "no", at: null })).type).toBe("error");
    expect(line(event(10, "error", { id: null, text: "say: nothing to say" })).type).toBe("error");
    expect(line(event(11, "something_new", {})).text).toContain("something_new");
  });

  it("shortens a child's name and leaves other names alone", () => {
    expect(childLabel("flare-home-fork-11")).toBe("fork 11");
    expect(childLabel("flare-home")).toBe("flare-home");
  });
});
