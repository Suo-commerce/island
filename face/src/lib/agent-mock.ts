import { W } from "./mock";

export const agentIdentity = {
  name: "flare-home",
  key: W.agent.id,
  state: "forked" as const,
  task: "Resolve deadline conflicts in taso planner",
  turn: 14,
  grantedSeq: 1104,
  grantedWhen: "2026-08-20",
  grantedBy: W.laptop,
};

export const outwardPolicy = [
  { kind: "say", rule: "auto", note: "replies to the session go out without asking", since: 1104 },
  { kind: "verse", rule: "approval", note: "every verse waits for a human approval, sent once", since: 1104 },
  { kind: "access", rule: "recorded", note: "every model call and read is written to the access stream", since: 1104 },
  { kind: "override", rule: "recorded, passes nothing", note: "override requests are counted and carry no content", since: 1833 },
];

export const agentBudget = {
  slotsTotal: 8,
  slotsUsed: 3,
  callsToday: 63,
  callsCap: 400,
  inKB: 947,
  outKB: 117,
  outCapKB: 2048,
  maxTurns: 24,
  maxForks: 2,
  forkAdopt: "approval" as const,
};

export const agentStreams = [
  { stream: "taso", read: true, write: "via approval" },
  { stream: "runo", read: true, write: "via approval" },
  { stream: "flare", read: true, write: "via approval" },
  { stream: "access", read: false, write: "auto" },
  { stream: "plan", read: true, write: "no" },
  { stream: "island", read: true, write: "no" },
];

export const agentConfigHistory = [
  { seq: 1833, when: "today 16:08", text: "override requests: recorded, counted, pass nothing" },
  { seq: 1802, when: "today 09:02", text: "phone key narrowed: −policy −grant (approvals still work)" },
  { seq: 1215, when: "2026-09-14", text: "artefact refs must have a receipt (R4)" },
  { seq: 1104, when: "2026-08-20", text: "agent key granted · outward policy set: say=auto, verse=approve" },
];
