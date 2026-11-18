import { describe, expect, it } from "vitest";

import { bytes, count, inForce, mayApprove, keysOf, ledgerOf, policySteps, singable, verseMax } from "@/lib/island";
import type { GrantLine, LogState } from "@/lib/session";
import { verseCommand } from "@/lib/sing";

const root = "aa".repeat(32);
const appender = "bb".repeat(32);
const laptop = "cc".repeat(32);
const tablet = "dd".repeat(32);
const at = 1_794_000_000_000_000;
const grant = (over: Partial<GrantLine> & Pick<GrantLine, "id" | "seq" | "holder">): GrantLine => ({
  at: at + over.seq * 1e6, by: root, scope: ["authority"], rights: ["grant"], role: "ordinary", narrows: null, ended_at: null, ended_by: null, ...over,
});

const state: LogState = {
  head: { seq: 12, hash: "ab".repeat(32) }, entries: 13, root, appender, state_hash: "cd".repeat(32), problem: null, now: at + 20e6,
  kinds: { verse: 3, grant: 4 },
  streams: [{ name: "authority", entries: 6, last: 9, last_at: at }, { name: "flare", entries: 1, last: 12, last_at: at }, { name: "approvals", entries: 2, last: 11, last_at: at }],
  grants: [
    grant({ id: "1.0", seq: 1, holder: root, rights: ["grant", "revoke", "policy"], scope: ["authority", "policy"], role: "root" }),
    grant({ id: "1.1", seq: 1, holder: appender, rights: ["verdict"], scope: ["gate"], role: "appender" }),
    grant({ id: "3.0", seq: 3, holder: laptop, rights: ["verse", "policy"], scope: ["flare", "taso"], ended_at: 6, ended_by: "6.0" }),
    grant({ id: "4.0", seq: 4, holder: tablet, rights: ["verse"], scope: ["flare"], ended_at: 9, ended_by: null }),
    grant({ id: "6.0", seq: 6, holder: laptop, rights: ["verse"], scope: ["flare", "taso"], narrows: "3.0" }),
    grant({ id: "9.0", seq: 9, holder: root, rights: ["revoke"], scope: ["authority"] }),
  ],
  policy: [
    { seq: 2, at, evaluator: "0da446e14d", invariants: ["VerseNamesEvidence", "VerseTextMax(4096)"] },
    { seq: 7, at, evaluator: "0da446e14d", invariants: ["VerseNamesEvidence", "VerseTextMax(2000)", "ArtefactHasRef"] },
    { seq: 8, at, evaluator: "9f0332d000", invariants: ["VerseNamesEvidence", "VerseTextMax(2000)", "ArtefactHasRef"] },
  ],
  refused: 0, verdicts: [],
  approvals: [
    { seq: 11, at, by: laptop, request: "11".repeat(32), verdict: "approve", used_by: 12 },
    { seq: 10, at, by: laptop, request: "22".repeat(32), verdict: "decline", used_by: null },
    { seq: 5, at, by: laptop, request: "33".repeat(32), verdict: "approve", used_by: null },
  ],
  dispatches: [{ seq: 12, at, by: appender, vessel: "91".repeat(16), op: 7, request: "11".repeat(32), approval: 11 }],
  vessels: [],
  writers: [{ key: laptop, entries: 3, last: 11, last_at: at + 11e6, counter: 3 }, { key: root, entries: 7, last: 9, last_at: at + 9e6, counter: 7 }],
};

describe("the keys", () => {
  const keys = keysOf(state);

  it("come root first, then the appender, then by first grant, the revoked last", () => {
    expect(keys.map((k) => [k.key.slice(0, 2), k.status])).toEqual([["aa", "root"], ["bb", "appender"], ["cc", "narrowed"], ["dd", "revoked"]]);
  });

  it("hold what their grants in force say", () => {
    expect(keys[2]?.holds).toEqual([{ rights: ["verse"], scope: ["flare", "taso"], grant: "6.0" }]);
    expect(keys[3]?.holds).toEqual([]);
    expect(keys[0]?.holds).toHaveLength(2);
  });

  it("have their history in the log's order, with the revoke at its entry", () => {
    expect(keys[2]?.history.map((h) => [h.seq, h.what])).toEqual([
      [3, "granted verse policy on flare taso"],
      [6, "narrowed (grant 3.0) to verse on flare taso"],
    ]);
    expect(keys[3]?.history.at(-1)).toMatchObject({ seq: 9, what: "grant 4.0 revoked" });
  });

  it("say when a key last wrote, and nothing for one that never did", () => {
    expect(keys[2]?.wrote).toEqual({ entries: 3, last: 11, at: at + 11e6 });
    expect(keys[1]?.wrote).toBeUndefined();
  });
});

describe("the policy", () => {
  it("is told as what each entry changed", () => {
    expect(policySteps(state.policy).map((s) => s.changes)).toEqual([
      ["+ VerseNamesEvidence", "+ VerseTextMax(4096)"],
      ["VerseTextMax(4096) → VerseTextMax(2000)", "+ ArtefactHasRef"],
      ["judged by build 9f0332d"],
    ]);
  });

  it("dates each invariant in force from the entry that made it read as it does", () => {
    expect(inForce(state.policy)).toEqual([
      { invariant: "VerseNamesEvidence", since: 2 },
      { invariant: "VerseTextMax(2000)", since: 7 },
      { invariant: "ArtefactHasRef", since: 7 },
    ]);
    expect(inForce([])).toEqual([]);
  });

  it("gives the verse's bound, if there is one", () => {
    expect(verseMax(state)).toBe(2000);
    expect(verseMax({ ...state, policy: [] })).toBeNull();
    expect(verseMax(null)).toBeNull();
  });
});

describe("who may approve", () => {
  it("is a key with approval on approvals in force, and no other", () => {
    const approver = "ee".repeat(32);
    const granted = { ...state, grants: [...state.grants,
      grant({ id: "20.0", seq: 20, holder: approver, rights: ["approval"], scope: ["approvals"] }),
      grant({ id: "21.0", seq: 21, holder: tablet, rights: ["approval"], scope: ["approvals"], ended_at: 22 }),
      grant({ id: "23.0", seq: 23, holder: laptop, rights: ["approval"], scope: ["flare"] })] };
    expect([approver, tablet, laptop, root].map((key) => mayApprove(granted, key))).toEqual([true, false, false, false]);
    expect(mayApprove(null, approver)).toBe(false);
  });
});

describe("the ledger", () => {
  it("pairs each approval with the dispatch that used it", () => {
    const rows = ledgerOf(state);
    expect(rows.map((r) => [r.approval.seq, r.outcome, r.dispatch?.seq])).toEqual([[11, "sent", 12], [10, "declined", undefined], [5, "approved, not used", undefined]]);
  });
});

describe("streams and sizes", () => {
  it("offers the streams people sing on, with those a verse grant names, and not the system's own", () => {
    expect(singable(state)).toEqual([{ name: "flare", entries: 1 }, { name: "taso", entries: 0 }]);
    expect(singable(null)).toEqual([]);
  });

  it("counts with the right noun", () => {
    expect([count(0, "task"), count(1, "task"), count(2, "entry", "entries")]).toEqual(["0 tasks", "1 task", "2 entries"]);
  });

  it("writes byte counts short", () => {
    expect([0, 947, 1200, 12_400, 3_100_000].map(bytes)).toEqual(["0 B", "947 B", "1.2 KB", "12 KB", "3.1 MB"]);
  });
});

describe("the terminal's command for a verse", () => {
  it("names the stream and the evidence and puts the text on one line", () => {
    expect(verseCommand("flare", "decision", "  Keep grant ids\n as pairs.\n\n")).toBe("/verse @flare decision Keep grant ids as pairs.");
  });

  it("is not there until there is a stream and something to sing", () => {
    expect(verseCommand("flare", "decision", "  \n ")).toBeNull();
    expect(verseCommand("", "decision", "words")).toBeNull();
  });
});
