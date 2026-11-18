// What the Island, Agent and Sing screens show, worked out from the log's
// state as the server gives it (GET /log/state). Pure functions.

import type { ApprovalLine, DispatchLine, GrantLine, LogState, PolicyLine } from "./session";

const two = (n: number) => String(n).padStart(2, "0");

/** A day and time, from microseconds since the epoch: today's as a time, older as a date. */
export function when(micros: number, now: number = Date.now() * 1000): string {
  const date = new Date(micros / 1000);
  const today = new Date(now / 1000);
  const time = `${two(date.getHours())}:${two(date.getMinutes())}`;
  const same = date.getFullYear() === today.getFullYear() && date.getMonth() === today.getMonth() && date.getDate() === today.getDate();
  return same ? time : `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())} ${time}`;
}

/** A count of bytes, short: `947 B`, `12.4 KB`, `3.1 MB`. */
export function bytes(n: number): string {
  if (n < 1000) return `${n} B`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)} KB`;
  return `${(n / 1_000_000).toFixed(1)} MB`;
}

export type KeyStatus = "root" | "appender" | "active" | "narrowed" | "revoked";
export type KeyRow = {
  key: string;
  status: KeyStatus;
  /** What it may write where, by its grants in force. */
  holds: { rights: string[]; scope: string[]; grant: string }[];
  history: { what: string; seq: number; at?: number | undefined }[];
  /** Its newest entry in the log, if it ever wrote. */
  wrote?: { entries: number; last: number; at: number } | undefined;
};

function grantText(grant: GrantLine): string {
  return `${grant.rights.join(" ") || "nothing"} on ${grant.scope.join(" ") || "no stream"}`;
}

/** Every key that holds or held a grant: the root and the appender first, then by first grant. */
export function keysOf(state: LogState): KeyRow[] {
  const rows = new Map<string, KeyRow>();
  const ended = new Map(state.grants.map((g) => [g.id, g]));
  for (const grant of state.grants) {
    const row = rows.get(grant.holder) ?? { key: grant.holder, status: "revoked" as KeyStatus, holds: [], history: [] };
    rows.set(grant.holder, row);
    row.history.push({ what: `${grant.narrows ? `narrowed (grant ${grant.narrows}) to` : "granted"} ${grantText(grant)}`, seq: grant.seq, at: grant.at });
    if (grant.ended_at === null) {
      row.holds.push({ rights: grant.rights, scope: grant.scope, grant: grant.id });
    } else if (grant.ended_by === null) {
      row.history.push({ what: `grant ${grant.id} revoked`, seq: grant.ended_at });
    }
  }
  for (const row of rows.values()) {
    const mine = state.grants.filter((g) => g.holder === row.key && g.ended_at === null);
    row.status = mine.some((g) => g.role === "root") ? "root"
      : mine.some((g) => g.role === "appender") ? "appender"
      : mine.length === 0 ? "revoked"
      : mine.some((g) => g.narrows !== null && ended.has(g.narrows)) ? "narrowed"
      : "active";
    row.history.sort((a, b) => a.seq - b.seq);
    const wrote = state.writers.find((w) => w.key === row.key);
    row.wrote = wrote ? { entries: wrote.entries, last: wrote.last, at: wrote.last_at } : undefined;
  }
  const rank: Record<KeyStatus, number> = { root: 0, appender: 1, active: 2, narrowed: 2, revoked: 3 };
  return [...rows.values()].sort((a, b) => rank[a.status] - rank[b.status] || (a.history[0]?.seq ?? 0) - (b.history[0]?.seq ?? 0));
}

export type PolicyStep = { seq: number; at: number; evaluator: string; changes: string[] };

const base = (invariant: string) => invariant.replace(/\(.*\)$/, "");

/** Each policy entry as what it changed against the one before. */
export function policySteps(policy: readonly PolicyLine[]): PolicyStep[] {
  return policy.map((entry, index) => {
    const before = index === 0 ? [] : (policy[index - 1]?.invariants ?? []);
    const changes: string[] = [];
    for (const invariant of entry.invariants) {
      const old = before.find((b) => base(b) === base(invariant));
      if (old === undefined) changes.push(`+ ${invariant}`);
      else if (old !== invariant) changes.push(`${old} → ${invariant}`);
    }
    for (const old of before) {
      if (!entry.invariants.some((i) => base(i) === base(old))) changes.push(`− ${old}`);
    }
    const judge = index > 0 && policy[index - 1]?.evaluator !== entry.evaluator ? [`judged by build ${entry.evaluator.slice(0, 7)}`] : [];
    return { seq: entry.seq, at: entry.at, evaluator: entry.evaluator, changes: changes.length || judge.length ? [...changes, ...judge] : ["no change to the invariants"] };
  });
}

/** The entry at which each invariant in force was first put in force as it now reads. */
export function inForce(policy: readonly PolicyLine[]): { invariant: string; since: number }[] {
  const now = policy.at(-1);
  if (!now) return [];
  return now.invariants.map((invariant) => {
    let since = now.seq;
    for (let i = policy.length - 1; i >= 0; i--) {
      if (policy[i]?.invariants.includes(invariant)) since = policy[i]?.seq ?? since;
      else break;
    }
    return { invariant, since };
  });
}

/** The most bytes a verse may hold, if the policy bounds it. */
export function verseMax(state: LogState | null): number | null {
  const bound = state?.policy.at(-1)?.invariants.map((i) => /^VerseTextMax\((\d+)\)$/.exec(i)).find(Boolean);
  return bound ? Number(bound[1]) : null;
}

export type LedgerRow = { approval: ApprovalLine; dispatch: DispatchLine | undefined; outcome: "sent" | "approved, not used" | "declined" };

/** Each approval with the dispatch that used it. */
export function ledgerOf(state: LogState): LedgerRow[] {
  return state.approvals.map((approval) => {
    const dispatch = approval.used_by === null ? undefined : state.dispatches.find((d) => d.seq === approval.used_by);
    return { approval, dispatch, outcome: approval.verdict === "decline" ? "declined" : approval.used_by === null ? "approved, not used" : "sent" };
  });
}

/** The streams a person sings on: the system's own streams are written by its parts. */
const SYSTEM = new Set(["authority", "policy", "approvals", "access", "gate", "receipts"]);
export function singable(state: LogState | null): { name: string; entries: number }[] {
  const named = new Map<string, number>();
  for (const stream of state?.streams ?? []) named.set(stream.name, stream.entries);
  for (const grant of state?.grants ?? []) {
    if (grant.ended_at === null && grant.rights.includes("verse")) for (const stream of grant.scope) if (!named.has(stream)) named.set(stream, 0);
  }
  return [...named].filter(([name]) => !SYSTEM.has(name)).map(([name, entries]) => ({ name, entries })).sort((a, b) => a.name.localeCompare(b.name));
}

/** A count with its noun: `1 task`, `3 tasks`. */
export function count(n: number, noun: string, plural: string = `${noun}s`): string {
  return `${n} ${n === 1 ? noun : plural}`;
}
