import { W } from "./mock";

export const integrity = {
  head: 1840, headHash: "f3a90c1e7b21de04e90c4b7a0d55f2a8c4418e3b9a7712aa5c03e6d1b2f7",
  verifiedAt: "today 21:58", entriesChecked: 1840,
  rootKey: "11c0ffee", appender: W.observer.id, stateHash: "9d2e44a1c07b3f58e61a0b9cd3427ef1",
};

export type KeyRow = { id: string; label: string; status: "active" | "narrowed" | "revoked" | "root"; streams: string; kinds: string; history: { what: string; seq: number; when: string }[] };
export const keys: KeyRow[] = [
  { id: "11c0ffee", label: "root (offline)", status: "root", streams: "*", kinds: "grant, revoke, policy", history: [{ what: "genesis", seq: 1, when: "2025-03-02" }] },
  { id: W.laptop.id, label: "laptop", status: "active", streams: "*", kinds: "verse, preference, policy, grant, revoke", history: [{ what: "granted", seq: 4, when: "2025-03-02" }] },
  { id: W.phone.id, label: "phone", status: "narrowed", streams: "*", kinds: "approval, verse", history: [{ what: "granted", seq: 212, when: "2025-06-11" }, { what: "narrowed: −policy −grant", seq: 1802, when: "today 09:02" }] },
  { id: W.agent.id, label: "flare-home agent", status: "active", streams: "taso, runo, flare, access", kinds: "verse (via approval), access", history: [{ what: "granted", seq: 1104, when: "2026-08-20" }] },
  { id: W.observer.id, label: "observer", status: "active", streams: "repos", kinds: "receipt", history: [{ what: "granted", seq: 18, when: "2025-03-04" }] },
  { id: W.gate.id, label: "gate", status: "active", streams: "gate", kinds: "verdict", history: [{ what: "granted", seq: 19, when: "2025-03-04" }] },
  { id: "55e0a9d3", label: "old-tablet", status: "revoked", streams: "—", kinds: "—", history: [{ what: "granted", seq: 640, when: "2025-11-01" }, { what: "revoked", seq: 1820, when: "today 12:40" }] },
];

export const rules = [
  { id: "R1", text: "verses name their evidence", since: 22 },
  { id: "R2", text: "verse text at most 4096 bytes", since: 23 },
  { id: "R3", text: "artefact verses carry a ref", since: 488 },
  { id: "R4", text: "artefact refs have a receipt", since: 1215 },
  { id: "R5", text: "agent verses need an approval entry", since: 1104 },
  { id: "R6", text: "override requests are recorded, counted, pass nothing", since: 1833 },
];
export const policyHistory = [
  { seq: 22, when: "2025-03-02", kind: "tighten", text: "+R1 verses name their evidence" },
  { seq: 23, when: "2025-03-02", kind: "tighten", text: "+R2 verse text ≤ 8192 bytes" },
  { seq: 301, when: "2025-07-19", kind: "tighten", text: "R2 8192 → 4096 bytes" },
  { seq: 488, when: "2025-09-30", kind: "tighten", text: "+R3 artefact verses carry a ref" },
  { seq: 702, when: "2026-01-12", kind: "refused", text: "loosen R2 to 6000 bytes — refused by ratchet" },
  { seq: 1104, when: "2026-08-20", kind: "tighten", text: "+R5 agent verses need approval" },
  { seq: 1215, when: "2026-09-14", kind: "tighten", text: "+R4 artefact refs have a receipt" },
  { seq: 1590, when: "2026-10-28", kind: "refused", text: "drop R4 for runo — refused by ratchet" },
  { seq: 1833, when: "today 16:08", kind: "tighten", text: "+R6 overrides pass nothing" },
];
export const overrideCount = { total: 3, last30: 1 };

export const verdicts = [
  { seq: 1834, when: "16:20", stream: "flare", rule: "R2", text: "verse too long, 4410 > 4096 bytes", writer: W.laptop },
  { seq: 1822, when: "13:18", stream: "island", rule: "R2", text: "verse too long, 5120 > 4096 bytes", writer: W.laptop },
  { seq: 1809, when: "10:03", stream: "runo", rule: "R1", text: "names no evidence", writer: W.agent },
  { seq: 1761, when: "yesterday", stream: "taso", rule: "R4", text: "ref 4e1a9f2 has no receipt", writer: W.agent },
  { seq: 1702, when: "2 days ago", stream: "plan", rule: "R5", text: "agent verse without approval entry", writer: W.agent },
];

export const vessels = [
  { name: "flare-home", state: "forked", slotsUsed: 3, slotsTotal: 8, seal: "18:02", authority: "blocked until w-41a settles", calls: 63, budget: 400 },
  { name: "fork 1 · priority-weights", state: "working", slotsUsed: 0, slotsTotal: 0, seal: "17:58", authority: "none (fork)", calls: 21, budget: 0 },
  { name: "fork 2 · log-order", state: "done", slotsUsed: 0, slotsTotal: 0, seal: "17:22", authority: "none (fork)", calls: 18, budget: 0 },
];

export const ledger = [
  { approval: 1836, by: "phone", outcome: "approved", action: "req 77fa…e2 verse→island", entry: 1836 as number | null, note: "re-proposed after lapse" },
  { approval: 1835, by: "server", outcome: "lapsed", action: "req 77fa…e2 verse→island", entry: null, note: "server restart before approval" },
  { approval: 1827, by: "phone", outcome: "declined", action: "req 2d71…c4 verse→plan", entry: null, note: "never sent" },
  { approval: 1817, by: "laptop", outcome: "approved", action: "req 81ab…0f verse→taso", entry: 1816, note: "sent once" },
  { approval: 1810, by: "phone", outcome: "approved", action: "req 3c9e…a1 say→session", entry: null, note: "say: delivered to session" },
];

export const repos = [
  { repo: "taso", stream: "taso", branch: "main", head: "9c3e51b", seq: 1830, uncovered: 7 },
  { repo: "island", stream: "island", branch: "main", head: "d12f8a0", seq: 1824, uncovered: 4 },
  { repo: "flare", stream: "flare", branch: "ui", head: "3fe9a77", seq: 1838, uncovered: 7 },
  { repo: "runo", stream: "runo", branch: "main", head: "6aa214e", seq: 1818, uncovered: 0 },
];

export const services = [
  { name: "broker", up: true, note: "uptime 11d" },
  { name: "session server", up: true, note: "3 windows attached" },
  { name: "sender", up: true, note: "queue 0 · last dispatch 16:58" },
  { name: "Warden", up: false, note: "restarting · down 40s" },
];
export const devices = [
  { label: "laptop", key: W.laptop.id, seen: "now", backup: "21:40" },
  { label: "phone", key: W.phone.id, seen: "18:03", backup: "replica 17:55" },
  { label: "home-server", key: "—", seen: "now", backup: "replica 22:00" },
  { label: "old-tablet", key: "55e0a9d3", seen: "41 days ago", backup: "—" },
];
