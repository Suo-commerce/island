# Technical spec — slice 1: a verse end to end

2026-11-09 · Jarmo

One repo, `island`, with the Island and Flare crates in one Cargo workspace. This spec covers only the first working slice: a verse typed in Flare's terminal travels through the broker and the gate into the signed Island log, and comes back to Flare's verified local replica. Vessels, approvals, providers and the agent loop wait for later slices.

## The slice

Slice 1 is the smallest path that touches every core part and is usable on day one: Jarmo writes a verse in Flare, and it comes back verified. It exercises the canonical encoding, the hash chain, writer and appender signatures, grants in the log, the gate, the single appender on the server, the broker's MCP face, Flare's shared session and the pulled replica. It deliberately leaves out Vessels, approvals, providers, inference and Flare's agent loop.

**Definition of done:**

- [ ] The `island` repo exists with one workspace holding the `island-*` and `flare-*` crates, the CI dependency rules and the banned-method lints
- [ ] The same submission encodes to the same bytes and hash on two machines; golden vectors are checked in
- [ ] Genesis: the root key pinned and declared, the bootstrap grants, then a root-signed `policy` entry with the starting invariants
- [ ] A grant for the daily key through `island-cli` with the root key from AgentVault; that grant narrowed in place once; a widening refused; a `revoke` that takes effect on the next submission
- [ ] `append_verse` from a local Claude or opencode session lands in the log with `seq` and `hash` returned
- [ ] A resent submission returns the original `seq` and `hash` and adds no entry; a replayed old one is refused
- [ ] A verse without an evidence kind, a verse outside the grant's scope and a verse with a bad signature are each refused, with a `verdict` entry for the gate refusal
- [ ] One real tightening during the week of use, one loosening refused, and every override request counted
- [ ] The appender survives `kill -9` at random points with no acknowledged entry lost; replay from genesis reproduces the state hash and every verdict
- [ ] `flare-link` pulls the log to the laptop, verifies the chain and both signatures against the pinned root key, and refuses a tampered record
- [ ] `flare-tui` attached to `flare-server` on the server shows the stream (`see`) and writes a verse (`propose`); a second `flare-tui`, run over `ssh` on the server, shows the same session events
- [ ] Harness: 10,000 seeds per CI run pass; every seed runs twice with identical traces; `island-model` agrees with the real gate and log on 10⁶ generated sequences; the stronger-means-subset oracle holds on every generated policy pair
- [ ] Used for real: Jarmo writes verses for a week through Flare and from at least one coding session

## Repo and dependency direction

One workspace, two halves, one rule: `flare-*` crates may depend on `island-core`, `island-log`'s reader and `island-client`, and no `island-*` crate ever depends on a `flare-*` crate. CI enforces it with cargo-deny, so the halves stay separable even though they live and change together. The Island binaries deploy to the server together with flare-server, while flare-tui and flare-link run on the laptop, all from one build.

*[Diagram in the source doc: the slice across two repos · seven numbered steps]*

**Layout:** `crates/island-*/` and `crates/flare-*/`, one `docs/spec.md` (this document), `harness/seeds/` for the archive, and a `justfile` with `check` (fmt, clippy with the bans, dependency rules), `test`, `harness SEEDS=N` and `init` for genesis. Taso changes (S2, and whatever promotion needs) go to the Taso repo as pull requests, never as a fork inside `island`.

## Record formats

A writer signs what it submits; the appender adds position and time and signs the whole entry. Two signatures, because the writer cannot know `seq`, `at` or `prev` before the appender assigns them, and the replica must still verify both who wrote and who ordered.

Encoding is `taso-core`'s canonical form, reused as is: fixed-width little-endian integers, byte strings as `u64` length plus bytes, enums as a `u8` tag plus fields, options as tag `0` or `1`, sequences as `u64` count plus elements, sets as sequences in ascending encoded order. No field names, no version byte; the domain string carries the version.

```rust
// island-core

pub struct Submission {
    stream: StreamId,        // bytes, the project or system stream
    writer: [u8; 32],        // writer's ed25519 public key; one key per device or session
    counter: u64,            // this writer's own count: 1, 2, 3, ...
    kind: Kind,              // u8 tag, below
    body: Body,              // kind-specific
}
// sub_hash   = blake3("island.submission.v1" || canon(Submission))
// sig_writer = ed25519_sign(writer_secret, sub_hash)

pub struct Entry {
    seq: u64,                // gap-free from 0
    at: u64,                 // microseconds since the epoch, appender clock, monotonic
    policy: u64,             // seq of the policy entry in force when judged
    prev: [u8; 32],          // hash of entry seq-1; zeros at genesis
    submission: Submission,
    sig_writer: [u8; 64],
}
// hash         = blake3("island.entry.v1" || canon(Entry))
// sig_appender = ed25519_sign(appender_secret, hash)
// record       = canon(Entry) || hash || sig_appender      (what is stored and shipped)
```

**Kinds in slice 1** (tags are stable; later kinds take the next free tag):

| Tag | Kind | Stream | Body |
| --- | --- | --- | --- |
| 0 | `policy` | `policy` | `{ evaluator: String, invariants: Vec<Invariant> }`; `evaluator` is the git commit of the build that judges |
| 1 | `grant` | `authority` | `{ holder: [u8;32], scope: Set<StreamId>, rights: Set<Kind>, narrows: Option<u64> }`; a grant's id is the `seq` of its entry |
| 2 | `revoke` | `authority` | `{ grant: u64 }`; effective from this entry's `seq` |
| 3 | `verse` | a project | `{ evidence: Evidence, text: String, refs: Vec<[u8;32]> }`; `text` is bounded by the `VerseTextMax` invariant |
| 4 | `preference` | a project or `plan` | `{ key: String, value: String }`; never ratcheted |
| 5 | `verdict` | `gate` | `{ rejected: Submission, sig_writer: [u8;64], reason: Refusal }` |
| 6, 7 | `receipt`, `access` | — | Reserved for slice 2 |
| 8 | `genesis` | `authority` | `{ root: [u8;32] }`; entry 0 only |
| 9 | `bootstrap` | `authority` | `{ grants: Vec<Grant> }`; entry 1 only |

`Evidence` is a `u8`: `decision` 0, `artefact` 1, `measurement` 2, `closed_question` 3.

**Invariants in slice 1.** Each judges new writes only, so tightening never invalidates history.

- `VerseNamesEvidence` (tag 0): no parameter.
- `VerseTextMax { bytes: u32 }` (tag 1): starts at 4096 and may only go down.
- `ArtefactHasRef` (tag 2): an artefact verse carries at least one ref. It is not in the first policy; adding it is the slice's real tightening.

**Keys.** Root: ed25519 in AgentVault, used only for genesis, `policy` entries and grants. Appender: ed25519 on the server, declared in the bootstrap entry. Writers: one ed25519 key per device or session, each with its own grant, so the log shows which device wrote what and one device can be revoked alone. In slice 1 these are the laptop's daily key, a server-side key for `ssh` sessions with a verses-only grant, and one key per coding-session installation. A Vessel's writer key is held by the broker in AgentVault: a Vessel holds no credentials, and its identity is its Warden-owned socket.

## Write path and gate

One appender, one thread, one order. The broker hands each submission to the appender, which runs these steps and acknowledges only after `sync` succeeds:

1. **Writer signature.** Verify `sig_writer` against `submission.writer`. Failure: refused with no entry; a forged submission is not worth a record.
2. **Counter.** Compare `counter` with the last one recorded for this writer. The next number goes on. The same number with the same `sub_hash` is a retry: answer with the original `seq` and `hash`, or the original refusal, and append nothing. Anything else (an old number, a gap, the same number with different content) is refused with no entry.
3. **Grant.** In the grant state at the next `seq`, find an unrevoked grant whose `holder` is the writer and whose scope and rights cover `stream` and `kind`. None: refused, no entry.
4. **Gate.** `judge(&state, &policy, &submission)`: every invariant in force must hold for the write. Failure: a `verdict` entry carrying the whole submission is appended instead, so replay can re-judge it. Either way the writer's counter advances.
5. **Position.** Assign `seq`, `at` (monotonic: never less than the previous entry's), `policy`, `prev`.
6. **Hash and sign.** Compute `hash`, sign as appender, encode the record.
7. **Durable.** `Storage::append` then `Storage::sync`. If `sync` fails the appender stops, replays from the last durable record on restart, and never acknowledges what it cannot prove durable.
8. **Acknowledge** with `seq` and `hash`.

**Authority and policy are read from the log itself.** There are no bearer tokens: a writer's signature is its credential, and its authority is whatever unrevoked grants name its key. Grant state is built by replaying `bootstrap`, `grant` and `revoke` entries; a `revoke` at `seq` n fails every submission judged after n. The policy in force is the newest `policy` entry. Biscuit tokens are left out on purpose and come back only if offline delegation is ever needed.

**Why the counter.** Every entry holds a complete signed submission, and the log is replicated and readable, so without a rule anyone could send an old submission again. A lost reply is the other case: the client cannot tell whether its verse landed. The counter makes a replay refusable and a retry safe, with one number of state per writer.

**The ratchet in slice 1:**

- **Policy.** A new `policy` entry is judged under the old one and must pass `strengthens`: every old invariant is still present, a parameter moves only in its declared direction, and nothing is removed. Additions are allowed.
- **What stronger means.** Whenever `strengthens(old, new)` holds, every write the new policy accepts is also accepted by the old one. The harness tests exactly this.
- **Grants.** A `grant` with `narrows: Some(id)` replaces grant `id` in place, and its scope and rights must be subsets of it. Widening is only ever a new root grant. Only root writes grants in slice 1.
- **Overrides are counted.** `island-cli override <sub_hash> <why>` records a `preference` entry with key `override-request`, naming a refused submission. The count is the Goodhart measure.
- **Evaluator.** The policy records the git commit of the judging build. Pinning and loading an exact build hash waits until builds are reproducible.

**Genesis** rests on one outside fact: every replica pins the root public key, or its hash, in its config. `island-cli init` then writes two entries:

- **Entry 0, kind `genesis` (tag 8):** `{ root: [u8;32] }`, signed by that key. It is valid only if `root` matches the pin.
- **Entry 1, kind `bootstrap` (tag 9):** `{ grants: Vec<Grant> }`, signed by the root key. A written bootstrap rule replaces the grant check for this entry only: its `seq` is 1, its writer is the root key declared in entry 0, and no other `bootstrap` entry is ever valid. It holds the initial grants: the root capability, and a `verdict` grant on the `gate` stream for a key separate from root.

Verdict entries are written under that `verdict` grant. Its holder is the appender's key, which is also the key `sig_appender` is checked against, so the appender's signatures on entries 0 and 1 are verified once entry 1 has been read. The first `policy` entry is entry 2, an ordinary root-signed write.

**State and replay.** `State { streams: BTreeMap<StreamId, StreamState>, grants: BTreeMap<u64, Grant>, writers: BTreeMap<[u8;32], Last>, policy: Policy }`, where `Last` holds a writer's latest counter, its `sub_hash` and the answer it got. `state_hash = blake3("island.state.v1" || canon(State))`. `replay(records)` verifies `prev`, `hash`, `sig_appender` and `sig_writer` on every record, re-runs `judge` on every `verdict`'s rejected submission and requires the same refusal, and returns the state and its hash. Any mismatch is a hard error naming the `seq`.

**Pure core.** `judge`, `strengthens`, `authorize` and `replay` live in `island-gate`, `island-auth` and `island-log`'s replay module with no async and no I/O; they take values and return values. The appender in `island-log` is the only place that touches `Clock`, `Rng` and `Storage`, and only through `island-env`.

## Interfaces

Three interfaces cross the repo boundary or a machine boundary. They are the contract between `island` and `flare`, so they are versioned and tested by golden fixtures in both repos.

**1. The broker's MCP tools** (`island-broker`, rmcp, streamable HTTP over the tailnet). There is no bearer token: the signed submission is the credential. The client builds and signs the `Submission`; the broker never holds a device's writer key. In slice 1, reads are open to any client on the tailnet.

| Tool | Input | Output |
| --- | --- | --- |
| `append_verse` | `{ submission: base64(canon(Submission)), sig_writer: base64 }` | `{ seq, hash }`, or a refusal with its reason; an exact resend gets the first answer again |
| `read_stream` | `{ stream, after?: u64, limit?: u32 }` (limit at most 500) | `{ records: [base64(record)], more: bool }` |
| `my_capabilities` | `{ writer: base64 }` | `{ grants: [{ id, scope, rights, narrows?, revoked_at? }], next_counter }` |

A small client library, `island-client`, builds submissions, signs them and decodes records. Flare and the coding-session tools both use it, so the wire format has one implementation per side.

**2. The replica pull** (`flare-link` → broker): `GET /log?after=<seq>&limit=<n>` returns records in order; `flare-link` verifies `prev`, `hash` and both signatures against the pinned root key and the two genesis entries, appends to a local append-only file `island.log`, and refuses the whole batch on the first bad record. `GET /head` returns the newest `seq` and `hash` so a client can tell how far behind it is. Pulls run on a timer and on demand before `see`.

**3. Flare's session protocol** (`flare-server` ↔ `flare-tui`, WebSocket, JSON). A session is an append-only event log on the server; a client attaches at a cursor and replays.

```
client → server   Attach  { session, after: u64 }
client → server   Command { id, verb: "see" | "propose", args }
server → client   Event   { seq, at, kind, payload }
   kinds: user_message, agent_text, verb_call, verb_result, error
```

The server assigns event order; two clients typing at once never conflict. In slice 1 `propose` carries a submission that the client signed with its own device key; the server relays it to `append_verse` and records the result as a `verb_result`. `see` is answered by `flare-server` from the log, through `island-log`'s reader. On the laptop, `flare-tui` also checks what it is shown against its own replica.

**One session, three ways in.** `flare-server` runs on the server, owns the session's event log and keeps running when every device is closed.

- **Laptop:** `flare-tui` attaches over the tailnet, signs with the laptop's daily key and cross-checks against its own verified replica.
- **Any other laptop:** `ssh` to the server and run the same `flare-tui` there. The key stays on the server with a verses-only grant, so a borrowed machine holds nothing.
- **Phone:** the web app served by `flare-server`, built after slice 1. The phone trusts the server's view. Approvals come only from the phone's passkey; for a high-stakes one, the request hash on the phone can be compared with the hash the laptop computed itself.

**The boundary inside the workspace.** Flare links only `island-core`, `island-log` (reader and replay) and `island-client`; everything else it reaches over MCP, `/log` and `/head`. A Flare change that needs a broker change is one commit in one repo, which is the agility gained; the dependency rule in CI is what keeps that from turning into a tangle.

## Harness for the slice

The harness is written with the first crate and runs on every push. In slice 1 the broker and appender share one process, so no network simulation is needed yet; the turmoil spike decides the runtime for slice 2, when the Vessel socket arrives.

**Generator** (`island-harness`, proptest). From a seed, a sequence of operations over three writers with distinct keys and capabilities:

- valid verses in and out of each writer's scope, with and without an evidence kind, on both sides of the `VerseTextMax` bound
- grants, narrowings, attempted widenings and revokes, including a revoke racing the next verse
- pairs of policies: tightened parameters, added invariants, removed invariants and loosened parameters
- bad writer signatures, an exact resend, a replayed old submission, a skipped counter, the same counter with different content
- storage faults: `sync` fails, a crash after `append` and before `sync`, a crash after acknowledgement

**Oracles**, checked after every operation:

1. The stored chain verifies: `prev`, `hash`, both signatures, gap-free `seq`.
2. `replay(prefix)` equals the live state hash, and re-judging every `verdict` gives the same refusal.
3. `island-model`, a few hundred lines with the same inputs, agrees on every accept and refuse.
4. No acknowledged entry is missing after a crash; no unacknowledged entry is treated as durable.
5. Nothing outside a writer's grants was ever accepted (hostile writers are in the generated set).
6. Stronger means subset: for every generated pair where `strengthens(old, new)` holds, no generated write is accepted by `new` and refused by `old`.
7. Retries are idempotent: an exact resend returns the first answer and adds no entry.

**Determinism.** `SimStorage`, `SimClock` and `SeededRng` implement `island-env`'s traits. Every seed runs twice and the full event traces are compared byte for byte; a difference fails the run and names the first diverging event.

**Seed archive.** A failing seed is shrunk by proptest and committed under `harness/seeds/` with the oracle it broke; CI replays every archived seed before the random ones. Nothing is ever removed from the archive.

## Deployment

Slice 1 runs without Taso. The server is set up Taso-ready from the start so spike S2 runs on it, and Vessels arrive in slice 2. The Taso facts below were checked against Taso main on 2026-11-09.

**Server.** UpCloud is Taso's most-tested host: its nightly release gate and soak runs happen on an UpCloud KVM server in Helsinki. Island takes the same: a Premium plan in Helsinki, Ubuntu 26.04, 4 vCPU and 16 GB to start, with a separate MaxIOPS volume of about 160 GB at `/var/lib/taso` for the block log, images and build output. It must be a KVM virtual machine with its own kernel: Taso needs `ublk_drv`, cgroup v2, no SELinux, `runc` and a local filesystem, so shared-kernel containers and managed Kubernetes are out. `taso-ctl init` checks every requirement before writing anything.

| Runs on the server | Stays off the server |
| --- | --- |
| `taso-blkd`, `taso-wardend` | AgentVault with the root, data and Vessel writer keys |
| `island-broker` with the appender | The approval passkey, on the phone |
| `flare-server` | Inference, on its GPU box |
| Tailscale; no public ports | The Island log replica, on the laptop |

Taso's packs and the Island log are shipped to UpCloud's S3-compatible object storage as well as to the laptop. A VM disk's durability under power loss is the provider's promise, not something the Warden checks, so VM snapshots are a convenience only.

**Vessels (slice 2).** One image, `island-agent`, built like Taso's demo agent and unpacked to `/var/lib/taso/agent-island`: Ubuntu 26.04 base, the `flare-agent` worker binary, git, coreutils, ripgrep, and the mount targets Taso expects. No keys, no tokens, no network tools. How Taso behaves, and what Island does about it:

- **Identity by socket.** The Field socket and the sender socket are per Vessel and owned by the Warden; the agent can never list or report on actions. The island socket follows the same rule, so no request on it carries a token.
- **One outbox.** The broker is Taso's sender, not a parallel channel: it reads released actions on the sender socket, signs them with the Vessel's key and reports outcomes back. A verse from a Vessel is an action with provider `island`.
- **Effects.** A fork's actions are held by default; a config-born Vessel's follow its `effects` setting. An agent can never change either.
- **Forks.** The quota is the `forks.slots` list in the Vessel's entry in `/etc/taso/warden.json`, one named child per slot. Children inherit the parent's disk uid range over an idmapped mount.
- **Promotion** adopts the child's files and its held actions in one commit. There is no other mode.
- **Spec D91.** Taso guarantees nothing about anything the agent can write that is not the Warden's disk. The island socket is such a channel, so the broker records everything that crosses it as an `access` entry, and anything that changes state goes through Taso's actions instead.

**Coming in Taso, and what Island does until then:**

- **Proposals query.** A read-only `Proposals` query on the Warden's decider socket, also `taso-ctl proposals <vessel>`, lists a held Vessel's proposed actions with their exact request bytes and a sealed flag. Two rules: nothing is ever sent from this list, and an unsealed item can vanish. Flare's review and Runo's rhythm read it. Until it ships, S2 reads proposals only after promotion, on the parent's `Open` list.
- **The review gate lives in the broker.** Promotion is requested by the parent agent, never approved from outside, and in a dispatching parent it releases the child's actions at once. So the broker, as Taso's sender, sends only what Jarmo or Runo has approved and reports not-applied for the rest. An approval names the hash of the exact request bytes of a sealed proposal.
- **Inbox.** A per-Vessel inbox, enabled in config and off for forks by default. The outside sends `Put { id, body }` on a Warden-owned socket and the agent pulls with `Next`. Delivery is at-least-once, so replies go out as actions keyed by the task id. Limits: 1 MiB per task, 256 undelivered. No secrets in tasks. The parent takes no new task while forks it intends to promote are still working. Until it ships, a task enters at birth, as config arguments, or as files the parent writes before forking.

| Vessel | Born | Effects | Purpose |
| --- | --- | --- | --- |
| `island-agent` (spike) | Config entry | `held` | Proves fork, rollback and a held action surviving a Warden kill |
| `flare-home` | Config entry, long-lived | `held` until the spike passes | Flare's agent; asks `Fork` |
| `flare-home` forks | `forks.slots`, one child per slot | held, by Taso's rule | Sub-agent tasks |
| `island-test` | Config entry | `held` | Harness level 2 fault runs; never promoted |

**Order.** First slice 1, without Taso. Then spike S2 on the server: `taso-ctl init`, Taso's demo, then one `island-agent` Vessel with `effects: held` and the broker attached as its sender. Nothing is promoted until the spike has shown a fork, a rollback and a held action surviving a Warden kill. Then the slice 2 spec.

## Spikes

Three time-boxed checks run beside the build. Each ends in a one-paragraph note in the repo and a decision; running over the box is itself a result.

| Spike | Question | Box | Decides |
| --- | --- | --- | --- |
| S1 turmoil + mad-turmoil | Can a toy appender and client run under turmoil with the clock and randomness overridden, and pass the run-twice check? | 2 days | The simulation runtime for slice 2; madsim is the fallback |
| S2 Taso on the server | After `taso-ctl init` and the demo: does one `island-agent` Vessel with `effects: held` and the broker as sender show a fork, a rollback and a held action surviving a Warden kill? And can the runtime bind-mount a second, named socket from the Vessel's entry with the network namespace still loopback-only? | 3 days | Whether reads and inference go through an island socket or through Field requests; whether anything may be promoted |
| S3 webauthn-rs challenge | Can the server supply the WebAuthn challenge, derived from the request hash, through `webauthn-rs-core`? | half a day | Whether an approval verifies from the log alone, or the log stores the challenge-to-request mapping |

Promotion in Taso adopts the child's files and its held actions together, and no other mode exists. Whether Flare needs its sub-agents to write results outside the promoted state of `/work`, or Taso needs a new mode asked for as a change, is decided after S2 and before Flare's milestone 3.

## Build order

Each step leaves something that runs. Spikes S1 to S3 start in the first days, in parallel with steps 1 to 3.

1. **Repo skeleton.** One workspace with the `island-*` and `flare-*` crates as empty libraries, CI with the dependency rules (`island-gate` and `island-model` depend only on `island-core`; no `island-*` crate depends on a `flare-*` crate; `flare-*` depends on Island only through `island-core`, `island-log`'s reader and `island-client`) and the Clippy bans. `docs/spec.md` is this document.
2. **`island-core`.** Types, canonical encoding on `taso-core`, hashing, both signatures, the counter. Golden vectors committed: one submission, one entry, their bytes and hashes.
3. **`island-log` and `island-cli`.** Appender over `island-env` with the counter rule, replay, `island-cli init` (the genesis and bootstrap entries), `verify`, `replay`, `tail`. Crash tests with real files.
4. **`island-model` and the harness.** Model, generators, oracles 1, 2, 4 and 7, determinism check, seed archive. From here CI is red if the model and the code disagree.
5. **`island-auth`.** Grants keyed by writer key, narrowing in place, `revoke`, `authorize` at a position; oracle 5 joins the harness. Root-signed grants from AgentVault through `island-cli`.
6. **`island-gate`.** `judge`, the three invariants, `strengthens`, `verdict` entries, `island-cli override`; oracles 3 and 6 join.
7. **`island-broker` and `island-client`.** rmcp server with the three tools, `/log` and `/head`; the client library. First real verse from a coding session.
8. **`flare-link` and `flare-core`.** Replica pull with verification against the pinned root key; `see`.
9. **`flare-server` and `flare-tui`.** Session event log on the server, WebSocket attach and replay, `propose` signed with the device key. The laptop and an `ssh` session share one session.
10. **A week of real use** with one real tightening of the policy, then spike S2 on the server and the slice 2 spec: the Vessel socket, access entries, inference, and Flare's agent loop ported from Pi.

## Slice 2 preview: repos as witnesses

The first item of slice 2 gives Island read access to Jarmo's repositories, so a verse can be checked against what happened in the repo, and the repo can ask for a verse. It needs no Vessels, so it can start right after slice 1 and before spike S2 finishes.

**The observer.** A poller inside the broker reads each repo's branch heads with one read-only GitHub credential from AgentVault. The server has no public ports, so it polls and takes no webhooks. For each new head it submits a `receipt` (tag 6): `{ repo, branch, head: [u8;20], previous: Option<[u8;20]>, commits: u32, observed_at }`. Hashes and counts only, never code or commit messages. The observer is a writer like any other: its own key, and a grant limited to `receipt` on the `repos` stream.

**Verse to repo.** An artefact verse names a commit hash in `refs`. `ArtefactHasRef` is tightened into `ArtefactRefHasReceipt`: the gate accepts the verse only if the state holds a receipt covering that commit. The gate reads the receipt, never GitHub, so replay stays pure.

**Repo to verse.** `flare-server` compares receipts with verses. When a repo has moved and no verse points at the new commits, `see` shows a nudge: the repo, the commit count since the last verse, and the time since. Flare may offer a draft built from commit messages, fetched at that moment through the broker and recorded as an `access` entry. Jarmo corrects it and sings it; the verse is always his.

**Rules:**

- The nudge is a signal, never a gate. An unsung repo blocks nothing.
- A receipt proves that something changed. Only the verse says what it meant.
- Repos map to project streams through `preference` entries (`repo:<owner>/<name>` to a stream), so the mapping can change freely.
- Drafts start from commit messages only. Diffs go to the inference provider only after a separate decision, since the repos include work code.
- Only pushed work is visible. Uncommitted changes on the laptop are not.

**Also in slice 2,** after spike S2: the Vessel socket for reads and inference, `access` entries, the inbox and `Proposals` paths, and Flare's agent loop ported from Pi.
