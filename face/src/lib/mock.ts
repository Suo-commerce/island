// Sample data, from the design prototype. The screens that are not
// connected yet show it, marked as such; `npm run dev:mock` runs the whole
// page on it. Nothing here is read from Island.
import type { Line, Waiting } from "./view";

export type Kind = "verse" | "preference" | "policy" | "grant" | "revoke" | "verdict" | "receipt" | "approval" | "access";
export type Writer = { id: string; label: string };
export const W = {
  laptop: { id: "a3f09c1e", label: "laptop" },
  phone: { id: "7b21de04", label: "phone" },
  agent: { id: "e90c4b7a", label: "flare-home agent" },
  observer: { id: "0d55f2a8", label: "observer" },
  gate: { id: "c4418e3b", label: "gate" },
} satisfies Record<string, Writer>;

export type Entry = { seq: number; time: string; stream: string; kind: Kind; writer: Writer; content: string; verified: boolean; evidence?: string | undefined };

const raw: [string, string, Kind, Writer, string, string?][] = [
  ["09:02", "policy", "policy", W.laptop, "verses: max 4096 bytes, must name evidence"],
  ["09:02", "authority", "grant", W.laptop, "grant phone a3f→7b21: approve, sing"],
  ["09:05", "repos", "receipt", W.observer, "taso main → 4e1a9f2 (+3)"],
  ["09:11", "taso", "verse", W.laptop, "Settled on CRDT-free merge for the planner; ordering comes from the log.", "decision"],
  ["09:20", "access", "access", W.agent, "12 model calls · 41 reads · 188 KB in / 22 KB out"],
  ["09:34", "island", "verse", W.laptop, "Signature check now runs on every client before display.", "artefact"],
  ["09:40", "repos", "receipt", W.observer, "island main → b77c03d (+5)"],
  ["09:52", "flare", "preference", W.phone, "Phone shows waiting items first."],
  ["10:03", "gate", "verdict", W.gate, "refused: names no evidence (runo)"],
  ["10:15", "runo", "verse", W.laptop, "Metre detector agrees with hand labels on 91 of 100 lines.", "measurement"],
  ["10:31", "approvals", "approval", W.phone, "approved say→session, req 3c9e…a1"],
  ["10:44", "plan", "verse", W.laptop, "Is the fork budget per task or per life? Per life. Closed.", "closed-question"],
  ["11:00", "access", "access", W.agent, "9 model calls · 30 reads · 140 KB in / 18 KB out"],
  ["11:12", "repos", "receipt", W.observer, "flare ui → 19d0e6c (+2)"],
  ["11:26", "flare", "verse", W.laptop, "Session stream renders identically on phone and laptop.", "artefact"],
  ["11:40", "policy", "policy", W.laptop, "agent outward: say=auto, verse=approve"],
  ["11:58", "taso", "verse", W.agent, "Planner benchmark: 2.4 ms p50 for 1k tasks.", "measurement"],
  ["12:10", "approvals", "approval", W.laptop, "approved verse→taso, req 81ab…0f"],
  ["12:22", "repos", "receipt", W.observer, "runo main → 6aa214e (+1)"],
  ["12:40", "authority", "revoke", W.laptop, "revoke old-tablet 55e0…: all"],
  ["13:05", "island", "verse", W.laptop, "Append-only check rejects rewritten heads.", "artefact"],
  ["13:18", "gate", "verdict", W.gate, "refused: verse too long, 5120 > 4096 bytes (island)"],
  ["13:30", "access", "access", W.agent, "21 model calls · 66 reads · 302 KB in / 39 KB out"],
  ["13:47", "plan", "verse", W.phone, "Next week: observer for lapsed actions.", "decision"],
  ["14:02", "repos", "receipt", W.observer, "island main → d12f8a0 (+4)"],
  ["14:15", "runo", "preference", W.laptop, "Prefer Finnish examples in runo verses."],
  ["14:33", "taso", "verse", W.laptop, "Dropped the cron path; receipts drive nudges.", "decision", ],
  ["14:50", "approvals", "approval", W.phone, "declined verse→plan, req 2d71…c4"],
  ["15:04", "flare", "verse", W.agent, "Approval confirm step uses passkey on phone.", "artefact"],
  ["15:20", "access", "access", W.agent, "7 model calls · 18 reads · 96 KB in / 11 KB out"],
  ["15:36", "repos", "receipt", W.observer, "taso main → 9c3e51b (+7)"],
  ["15:51", "island", "verse", W.laptop, "Is server-side verification enough? No. Closed.", "closed-question"],
  ["16:08", "policy", "policy", W.laptop, "override requests: recorded, counted, pass nothing"],
  ["16:20", "gate", "verdict", W.gate, "refused: verse too long, max 4096 bytes (flare)"],
  ["16:35", "flare", "verse", W.laptop, "Log head shown with local verification mark.", "artefact"],
  ["16:52", "approvals", "approval", W.laptop, "lapsed verse→island (server restart)"],
  ["17:05", "runo", "verse", W.agent, "Stress model misreads compounds in 4% of lines.", "measurement"],
  ["17:21", "access", "access", W.agent, "14 model calls · 52 reads · 221 KB in / 27 KB out"],
  ["17:40", "repos", "receipt", W.observer, "flare ui → 3fe9a77 (+3)"],
  ["17:58", "taso", "verse", W.laptop, "Fork notes stored beside the session, not in the log.", "decision"],
];
export const entries: Entry[] = raw.map(([time, stream, kind, writer, content, evidence], i) => ({
  seq: 1801 + i, time, stream, kind, writer, content, evidence, verified: i % 9 !== 4,
})).reverse();

export const initialWaiting: Waiting[] = [
  { id: "w-41a", type: "action", at: "18:02", stream: "taso",
    text: "Planner now resolves conflicting deadlines by log order; 0 regressions across 214 fixtures.",
    provider: "island-log", hash: "5e0b…91c7", fork: "fork 2 · log-order" },
  { id: "w-41b", type: "nudge", at: "17:40", repo: "flare", commits: 7, since: "3 days ago" },
  { id: "w-41c", type: "verdict", at: "16:20", stream: "flare", reason: "verse too long, max 4096 bytes (got 4410)",
    draft: "Session view: chronological, device-tagged, identical on every window…" },
];

export const agent = {
  state: "forked" as const, task: "Resolve deadline conflicts in taso planner", turn: 14, slotsTotal: 8, slotsLeft: 5,
  forks: [
    { name: "fork 1 · priority-weights", done: false, note: "Trying weighted priorities; 3 fixtures failing." },
    { name: "fork 2 · log-order", done: true, note: "Order by log seq; all 214 fixtures pass. Drafted a verse." },
  ],
  usage: { calls: 63, inKB: 947, outKB: 117 },
};

export const initialSession: Line[] = ([
  ["16:40", "laptop", "msg", "Morning. Pick up the taso deadline conflicts."],
  ["16:40", "agent", "reply", "On it. Reading planner/ and the last 3 taso verses."],
  ["16:41", "laptop", "cmd", "/status"],
  ["16:41", "server", "result", "agent working · task taso-deadlines · turn 1 · slots 7/8"],
  ["16:44", "agent", "reply", "Two plausible approaches. Forking to try both."],
  ["16:44", "agent", "fork", "fork 1 · priority-weights started"],
  ["16:44", "agent", "fork", "fork 2 · log-order started"],
  ["16:52", "server", "error", "server restart · pending verse→island lapsed"],
  ["16:53", "agent", "action", "re-proposed verse→island (req 77fa…e2)"],
  ["16:58", "phone", "settled", "approved verse→island · sent once · #1836"],
  ["17:02", "phone", "msg", "How's fork 1 looking?"],
  ["17:02", "agent", "reply", "3 fixtures failing on overlapping weekends. Still working."],
  ["17:10", "laptop", "cmd", "/forks"],
  ["17:10", "server", "result", "fork 1 working · fork 2 working · slots 5/8"],
  ["17:22", "agent", "fork", "fork 2 · log-order: 214/214 fixtures pass · done"],
  ["17:24", "laptop", "msg", "Nice. Don't adopt yet, wait for fork 1."],
  ["17:24", "agent", "reply", "Understood. Holding fork 2's work."],
  ["17:31", "laptop", "cmd", "/usage today"],
  ["17:31", "server", "result", "63 calls · 947 KB in · 117 KB out"],
  ["17:45", "phone", "msg", "Ok, go with fork 2 if 1 isn't done by six."],
  ["17:45", "agent", "reply", "Will do."],
  ["18:01", "agent", "reply", "Fork 1 still failing. Adopting fork 2."],
  ["18:01", "agent", "fork", "adopted fork 2 · fork 1 will be discarded on finish"],
  ["18:02", "agent", "action", "verse→taso waiting for approval (req 5e0b…91c7)"],
  ["18:03", "phone", "msg", "Seen. Reviewing."],
] as const).map(([time, device, type, text], i) => ({ id: "s" + i, time, device, type, text }));

export const logHead = { seq: 1840, hash: "f3a90c1e7b21de04e90c4b7a0d55f2a8c4418e3b9a7712aa", verified: true };
