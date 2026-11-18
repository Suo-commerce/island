// A sample of the log's state, for the page started on sample data
// (`npm run dev:mock`): the same shape the server gives, with invented
// keys and entries. Nothing here is read from Island.
import type { LogState } from "./session";

const k = (two: string) => two.repeat(32);
const at = (seq: number) => 1_794_000_000_000_000 + seq * 3_600_000_000;
const root = k("11");
const appender = k("0b");
const laptop = k("a3");
const agent = k("e9");
const sender = k("5e");
const socket = k("50");
const vessel = "91".repeat(16);

export const sampleState: LogState = {
  head: { seq: 60, hash: k("f3") },
  entries: 61,
  root,
  appender,
  state_hash: k("9d"),
  problem: null,
  now: at(61),
  kinds: { genesis: 1, bootstrap: 1, policy: 2, grant: 5, verse: 31, verdict: 2, access: 12, approval: 4, dispatch: 3 },
  streams: [
    { name: "access", entries: 12, last: 59, last_at: at(59) },
    { name: "approvals", entries: 7, last: 58, last_at: at(58) },
    { name: "authority", entries: 7, last: 30, last_at: at(30) },
    { name: "flare", entries: 9, last: 60, last_at: at(60) },
    { name: "island", entries: 14, last: 52, last_at: at(52) },
    { name: "policy", entries: 2, last: 21, last_at: at(21) },
    { name: "taso", entries: 8, last: 44, last_at: at(44) },
  ],
  grants: [
    { id: "1.0", seq: 1, at: at(1), by: root, holder: root, scope: ["authority", "policy"], rights: ["grant", "revoke", "policy"], role: "root", narrows: null, ended_at: null, ended_by: null },
    { id: "1.1", seq: 1, at: at(1), by: root, holder: appender, scope: ["gate"], rights: ["verdict"], role: "appender", narrows: null, ended_at: null, ended_by: null },
    { id: "3.0", seq: 3, at: at(3), by: root, holder: laptop, scope: ["flare", "island", "taso"], rights: ["verse", "preference"], role: "ordinary", narrows: null, ended_at: null, ended_by: null },
    { id: "24.0", seq: 24, at: at(24), by: root, holder: sender, scope: ["approvals"], rights: ["dispatch"], role: "ordinary", narrows: null, ended_at: null, ended_by: null },
    { id: "25.0", seq: 25, at: at(25), by: root, holder: laptop, scope: ["approvals"], rights: ["approval"], role: "ordinary", narrows: null, ended_at: null, ended_by: null },
    { id: "28.0", seq: 28, at: at(28), by: root, holder: socket, scope: ["access"], rights: ["access"], role: "ordinary", narrows: null, ended_at: null, ended_by: null },
    { id: "30.0", seq: 30, at: at(30), by: root, holder: agent, scope: ["flare"], rights: ["verse"], role: "ordinary", narrows: null, ended_at: null, ended_by: null },
  ],
  policy: [
    { seq: 2, at: at(2), evaluator: "0da446e14d77", invariants: ["VerseNamesEvidence", "VerseTextMax(4096)"] },
    { seq: 21, at: at(21), evaluator: "0da446e14d77", invariants: ["VerseNamesEvidence", "VerseTextMax(2000)", "ArtefactHasRef"] },
  ],
  refused: 2,
  verdicts: [
    { seq: 47, at: at(47), stream: "flare", writer: laptop, kind: "verse", reason: "it breaks VerseTextMax(2000)", content: '[decision] "A verse that ran on for far too long…"' },
    { seq: 33, at: at(33), stream: "taso", writer: laptop, kind: "verse", reason: "it breaks ArtefactHasRef", content: '[artefact] "Built the planner"' },
  ],
  approvals: [
    { seq: 57, at: at(57), by: laptop, request: k("77"), verdict: "approve", used_by: 58 },
    { seq: 54, at: at(54), by: laptop, request: k("2d"), verdict: "decline", used_by: null },
    { seq: 49, at: at(49), by: laptop, request: k("81"), verdict: "approve", used_by: 50 },
    { seq: 40, at: at(40), by: laptop, request: k("3c"), verdict: "approve", used_by: 41 },
  ],
  dispatches: [
    { seq: 58, at: at(58), by: sender, vessel, op: 9, request: k("77"), approval: 57 },
    { seq: 50, at: at(50), by: sender, vessel, op: 6, request: k("81"), approval: 49 },
    { seq: 41, at: at(41), by: sender, vessel, op: 2, request: k("3c"), approval: 40 },
  ],
  vessels: [
    {
      vessel,
      inference_today: { calls: 63, bytes_in: 947_000, bytes_out: 117_000 },
      inference: { calls: 412, bytes_in: 6_100_000, bytes_out: 803_000 },
      providers: { melious: { calls: 398, bytes_in: 6_000_000, bytes_out: 790_000 }, stub: { calls: 14, bytes_in: 100_000, bytes_out: 13_000 } },
      reads: { calls: 0, bytes_in: 0, bytes_out: 0 },
      tasks: { calls: 37, bytes_in: 0, bytes_out: 9_400 },
      notes: { calls: 8, bytes_in: 2_100, bytes_out: 2_100 },
      last_access: 59,
      last_access_at: at(59),
      dispatched: 3,
    },
  ],
  writers: [
    { key: agent, entries: 3, last: 60, last_at: at(60), counter: 1 },
    { key: socket, entries: 12, last: 59, last_at: at(59), counter: 1 },
    { key: sender, entries: 3, last: 58, last_at: at(58), counter: 1 },
    { key: laptop, entries: 33, last: 57, last_at: at(57), counter: 1 },
    { key: root, entries: 8, last: 30, last_at: at(30), counter: 1 },
    { key: appender, entries: 2, last: 47, last_at: at(47), counter: 1 },
  ],
};
