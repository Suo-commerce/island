import { W } from "./mock";

export const VERSE_MAX_BYTES = 4096;

export const evidenceKinds = [
  { kind: "decision", hint: "a choice made, with the reason" },
  { kind: "artefact", hint: "something built — needs a ref with a receipt (R3, R4)" },
  { kind: "measurement", hint: "a number you measured, with how" },
  { kind: "closed-question", hint: "a question asked and answered, closed" },
] as const;

export const singableStreams = [
  { stream: "taso", note: "planner work" },
  { stream: "island", note: "the log itself" },
  { stream: "runo", note: "metre detector" },
  { stream: "flare", note: "this console" },
  { stream: "plan", note: "decisions and questions" },
];

export type Draft = {
  id: string;
  stream: string;
  evidence: (typeof evidenceKinds)[number]["kind"];
  text: string;
  updated: string;
  bytes: number;
};

export const drafts: Draft[] = [
  {
    id: "d-1",
    stream: "taso",
    evidence: "decision",
    text: "Deadline conflicts resolve by log order, not priority weights. Weights tempted us twice; both times a fixture caught a regression. Order is explainable in one sentence, weights were not.",
    updated: "18:11",
    bytes: 214,
  },
  {
    id: "d-2",
    stream: "flare",
    evidence: "artefact",
    text: "Commands wheel spins the chosen sector under the gold arrow before navigating. Ref pending — waiting on receipt for flare ui head.",
    updated: "17:58",
    bytes: 158,
  },
  {
    id: "d-3",
    stream: "runo",
    evidence: "measurement",
    text: "Compound stress errors cluster in loanwords: 11 of 14 misreads. Sampled 350 lines, hand-checked.",
    updated: "16:40",
    bytes: 103,
  },
];

export const sungRecently = [
  { seq: 1835, time: "17:58", stream: "taso", evidence: "decision", writer: W.laptop, text: "Fork notes stored beside the session, not in the log.", verified: true },
  { seq: 1829, time: "16:35", stream: "flare", evidence: "artefact", writer: W.laptop, text: "Log head shown with local verification mark.", verified: true },
  { seq: 1824, time: "15:51", stream: "island", evidence: "closed-question", writer: W.laptop, text: "Is server-side verification enough? No. Closed.", verified: true },
  { seq: 1816, time: "14:33", stream: "taso", evidence: "decision", writer: W.laptop, text: "Dropped the cron path; receipts drive nudges.", verified: false },
  { seq: 1807, time: "11:58", stream: "taso", evidence: "measurement", writer: W.agent, text: "Planner benchmark: 2.4 ms p50 for 1k tasks.", verified: true },
];
