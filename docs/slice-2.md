# Technical spec — slice 2: an agent's action end to end

2026-11-13 · decisions 2026-11-14

Slice 1 carried a verse Jarmo typed into the signed log and back. Slice 2 puts an agent beside him: Flare's agent lives in a Taso Vessel on the Island server, thinks with a model, tries alternatives in forks, and can act outward only through actions that Jarmo approves. It builds on slice 1 as merged and on spike S2, which ran every Taso behaviour this design leans on (`scripts/s2-spike.sh`, runs a to h and s, all passing on the server at Taso `c031ee5`).

Where this document differs from `docs/spec.md` (its "Deployment" section on Vessels and its two slice 2 previews), this document holds. Taso facts here were read in Taso main at `c031ee5`; each is marked *(ran)* if S2 ran it and *(read)* if it was only read in the code.

## The slice

The smallest path that touches every new part and is useful on its first day:

1. A repo of Jarmo's moves. The observer records a `receipt`; Flare shows a nudge: this repo has commits no verse speaks for.
2. Jarmo tells Flare's agent, in the shared session: "draft the verse for that".
3. The agent, in its Vessel `flare-home`, pulls the task, reads the log and the commit messages through the island socket, and talks to a model through the same socket.
4. It forks: each child drafts one alternative in its own copy of the state. Nothing a child does leaves its Vessel, except a short note saying what it made.
5. The agent promotes one child. The child's files and its proposed actions become the agent's own: a reply to Jarmo with the draft, and a verse of the agent's own saying what it did.
6. The reply is delivered to the session at once. The agent's verse waits: Flare shows it under "waiting for you" with its exact text.
7. Jarmo approves it from Flare. `island-sender` sends it once: signed with the Vessel's writer key, judged by the gate, appended. Every client sees it, verified against the log.
8. Jarmo corrects the draft and sings the repo's verse himself, with his own key. It names the commit, and the gate accepts it because a receipt covers that commit.

**Definition of done:**

- [ ] `taso-blkd` and `taso-wardend` run as services on the server from `/etc/taso/warden.json`; `flare-home` is a long-lived Vessel that survives a reboot and a `kill -9` of the Warden with nothing acknowledged lost
- [ ] `flare-agent` answers a message typed in `flare-tui`; the reply arrives as a session event every attached client sees
- [ ] A model call from a fork child works; a child's action of any kind is answered *held* and reaches nothing
- [ ] The agent forks two children for one task, reads their notes, promotes one, is restarted by the Warden on the child's files and finishes the task
- [ ] An agent verse waits until approved; approved, it lands once in the log under the Vessel's key; declined, it never lands; after a Warden kill and a broker restart neither rule breaks
- [ ] Every byte that crossed the island socket is counted in an `access` entry; no prompt or answer is in the Island log; the agent's transcript is in its Vessel's Chronicle
- [ ] The observer writes a `receipt` when a repo's branch head moves; an artefact verse naming a commit without a receipt is refused with a `verdict`
- [ ] The four new kinds have golden vectors; replay from genesis re-judges every verdict and reproduces the state hash with them
- [ ] Harness: the approval-and-dispatch model agrees with `island-sender` on generated sequences with sender crashes, Warden restarts and repeated listings; an action is never sent unapproved and never twice
- [ ] The server gate (`just vessel-gate`, grown from `s2-spike.sh`) passes with `flare-agent` driven by a scripted model
- [ ] Flare's face exists as a second client of the same session, built from Jarmo's UX plan; the terminal still does everything
- [ ] Used for real for two weeks: at least one repo verse a day drafted this way

**Left out on purpose:** mail and calendar, any provider besides the session and the Island log, Runo as an approver, Stormgate, review of a fork's exact actions before promotion, Taso's inbox, a second node, OpenBao Transit, shipping packs to object storage.

## Decisions

Decided by Jarmo on 2026-11-14 (D1, D2, D3, D5); D4 and D6 as recommended, since Taso offers no alternative today.

| | Decision | Decided |
| --- | --- | --- |
| **D1** | Order of the three parts | Agent path first, then repos; the face is built beside both from Jarmo's UX plan (in Lovable) |
| **D2** | Which model the server calls | Configurable providers, all speaking the OpenAI-compatible chat API: a local model, a hosted provider or Jarmo's own GPU, chosen in `island-socket`'s config and switchable without touching the agent ("Providers" below) |
| **D3** | What the agent may write | Its own stream `flare` only. A draft for a project stream comes back as a reply, and Jarmo sings it: "the verse is always his" stays true |
| **D4** | How a parent learns what its forks made | A short note per child through the island socket, counted as access, until Taso has a `Proposals` or `Children` query |
| **D5** | Who may approve | Any key holding the `approval` right; first Jarmo's laptop daily key. The passkey arrives as a policy tightening (`ApprovalHasPasskey`) once spike S3 has run |
| **D6** | Where model conversations are kept | In the Vessel's Chronicle (Taso `Transcript` records), sealed with the turn. The Island log holds counts and digests only |

## What S2 settled

Spike S2's note, as `docs/spec.md` asks for one. Taso `c031ee5` builds on the server and passes its own no-trace gate there. A dispatching Vessel's effects reach a sender exactly as declared; a fork child's reach nothing, read-only ones included; promotion releases exactly the promoted child's; a child killed after its Seal keeps its proposals and they are adopted once; a turn that never ended is withdrawn; a released action whose outcome is lost in a Warden kill blocks the Vessel until a sender settles it. A Unix socket placed in the Vessels' read-only root filesystem is reached by prime and child with Landlock on, and its peer uid names the caller. `island-sender` sends an approved action once across its own crash, a Warden kill and a broker restart. Decisions: the prime dispatches and forks hold; the sender is a root relay; thinking does not go through the Field.

## The parts

| Part | Runs as | Holds | New |
| --- | --- | --- | --- |
| `taso-blkd`, `taso-wardend` | root, services | the Vessels' state | as services |
| `flare-agent` | inside a Vessel, mapped uid | nothing: no key, no token, no network | yes |
| `island-socket` | user `island-socket` | the model endpoint's credential, the read-only GitHub credential, its two writer keys | yes |
| `island-sender` | root, service | the Vessel's writer key, its own key | grows |
| `island-broker` | user `island` | the appender key | new kinds |
| `flare-server` | user `island` | no key | protocol v2 |
| `flare-tui`, the face | Jarmo's devices | the device keys | verbs |

Five channels, and what each may carry:

| Channel | From → to | Carries | Recorded |
| --- | --- | --- | --- |
| Field socket | agent → Warden | turns, transcript, actions, fork, promote | the Vessel's Chronicle |
| Island socket | agent → `island-socket` | thinking, reading, task pull, fork notes | `access` entries (counts, digests) |
| Sender socket | Warden → `island-sender` | released actions, outcomes | Chronicle; `dispatch` entries |
| Broker | everyone → log | signed submissions | the Island log |
| Session | clients ↔ `flare-server` | messages, events, signed approvals | the session's event log |

**The rule between the first two.** What changes the world goes through the Field as an action, so Taso can hold it. What only informs the agent goes through the island socket. Taso guarantees nothing about the second channel (its spec, D91), so it is narrow, counted, and can change nothing in Island but its own access records.

## The Vessel and the agent

**The entry** in `/etc/taso/warden.json` (accepted by Taso's own parser):

```json
{
  "namespace": "151a4d00000000000000000000000052",
  "subject": "flare-home",
  "uid_slot": 10,
  "effects": "dispatch",
  "restart": "always",
  "forks": {"slots": [11, 12, 13, 14, 15, 16, 17, 18]},
  "runtime": {
    "rootfs": "/var/lib/taso/agent-island",
    "args": ["/usr/bin/flare-agent"],
    "env": ["ISLAND_SOCKET=/run/island/island.sock"],
    "memory_bytes": 2147483648,
    "pids": 256
  }
}
```

The namespace is the one S2 used; on a node with one tenant it is only a name. Slot 10 makes the prime's host uid 855360, and its first child's 920896.

**The image**, `/var/lib/taso/agent-island`, built without Docker the way `s2-spike.sh` builds its rootfs: the `flare-agent` binary, a shell, coreutils, git, ripgrep, their libraries, Taso's Landlock shim, the pinned root public key and the mount targets. The directory `run/island/` in it holds the island socket, placed there by `island-socket` on the host.

**Forks are a budget** *(read)*. A child takes the first slot no child of this Vessel has ever taken; a slot is never given twice, and a child is never restarted. Eight slots are eight forks for the life of `flare-home`, not eight at a time. The list can be lengthened in the config, and each slot is a uid range and a dataset that stays. Until Taso frees a slot (wish 5), the agent forks only when alternatives are worth it, shows the slots left, and `see` warns when two remain.

**Who am I.** A fork child runs the same image, command line and environment as its parent *(read)*, so the agent cannot tell from inside whether it is the prime. Its first request on the island socket is `whoami`, answered from the peer uid: prime or fork, and which slot.

**One run of the prime:**

1. `Hello` on the Field. The reply names the last sealed turn; the agent loads its own state from `/work/.flare/`, which is exactly what that Seal holds.
2. Pull the next task after its sealed cursor. No task: wait.
3. Work: call the model, run tools, record each model message as a Taso `Transcript` entry.
4. Declare actions (a reply, a verse). End the turn; Taso seals.
5. Back to 2.

**Turns.** A turn ends when the agent has replied or is waiting for Jarmo, and at least every two minutes of work, so a crash costs little. Every start rolls the files back to the last Seal *(ran)*, so the agent keeps nothing in memory it cannot rebuild from `/work`.

**Keys of actions are derived, never random:** `blake3(task id, purpose, index)`, 32 bytes. A turn redone after a crash declares its actions again, as new ops with the same keys. The session and the sender both treat a key they have already acted on as done, so Jarmo is neither told nor asked the same thing twice.

**Tools of the loop.** The loop is the port of Pi's: model call, tool calls, results, repeat. Its tools:

| Tool | Does | Leaves the Vessel as |
| --- | --- | --- |
| `read`, `write`, `edit`, `bash` | work in `/work` | nothing |
| `see` | read the Island log, a stream, the receipts | island socket, read |
| `repo_commits` | commit messages of a repo since a commit | island socket, read |
| `say` | reply to Jarmo | action `flare.say` |
| `verse` | propose a verse on a stream its grant covers | action `island.verse` |
| `fork` | start a child on a task | Field `Fork` |
| `promote` | adopt one child | Field `Promote` |

**Forking, step by step:**

**A prime that seals after a fork cannot promote that child** *(ran: S4, first run; Taso's rule M7, "this vessel sealed again since the fork")*. The child's work continues the prime's state at the fork Seal; once the prime has moved on, it no longer does. So from the first fork to the promotion the prime ends no turn: it forks every child from one Seal, waits, and promotes. A crash in that window costs only the waiting, since every child is forked from the Seal it rolls back to. The two-minute turn rule pauses for it.

1. The prime writes the children's tasks to `/work/.flare/fork-tasks.json`, ends the turn, and asks `Fork` once per child, each from that same newest Seal. The Seal must be shipped first, or the fork is refused *(read)*; each reply names the child and, in its name, the slot.
2. Each child starts, learns from `whoami` that it is a fork and in which slot, takes its task from the file by its place among the slots, does it, declares its actions (all held), ends its turn, posts a note (what it made, in at most 16 KiB, and that it is done) and exits. The task file stays in the child's files; the child removes it before its last Seal, so a promoted child's files carry no stale task.
3. The prime, still in the turn that forked, reads the notes, chooses, and asks `Promote`. Taso ends the run for the adoption; with `restart: always` the Warden starts the agent again on the child's files, about two seconds later, with Hello's last sealed turn the child's *(ran: S4)*. The prime's unsealed files are gone.
4. After the restart the child's adopted actions, and any action the prime released in the turn that promoted, are released and **unknown**, and the Vessel has no effect authority until they are settled *(ran: S4: "no effect authority: 2 unresolved effect(s)")*. An unknown action waits for its answer like any other (the order of decisions, below): approved before or after the promotion, it is sent; declined, it is settled. Until all are answered the agent proposes nothing new, so after a promotion it tells Jarmo what is waiting. The prime releases nothing in the turn that promotes.
5. Children not chosen stay as they are: sealed, held, sent nowhere. Their notes are dropped. To work on without promoting any, the prime ends its turn: the fork set is given up.
6. A slot is used once, and a finished child's dataset stays mounted while the Warden runs *(ran: S4)*: `flare-home`'s eight fork slots are eight forks for the life of its config. Giving it more slots is a config change; reclaiming them is a wish to Taso (wish 5).

**Limits.** 2 GiB of memory, 256 processes, no network *(ran)*. At most 16 actions waiting for approval; more are not proposed until some are answered, because Taso's outbox holds 8 MiB.

## The island socket

HTTP over a Unix socket at `/run/island/island.sock` in the Vessels' root filesystem. `island-socket` serves it on the host. The caller is its peer uid: `host_uid_base + slot × 65536` *(ran)*, looked up in `/etc/taso/warden.json`; a uid that is no Vessel's is refused. No request carries a token.

| Request | Answer | Counted as |
| --- | --- | --- |
| `GET /whoami` | `{ vessel, subject, role: "prime" \| "fork", slot, forks_left }` | — |
| `POST /v1/chat/completions` | the model's answer, streamed; the socket sets the credential and allows only the configured models | inference |
| `GET /task?after=<id>` | the next message Jarmo addressed to the agent, or nothing | task |
| `GET /log/head`, `/log/stream/<name>`, `/log/receipts` | records, verified by the agent's `island-client` against the pinned root | read |
| `GET /repo/<owner>/<name>/commits?since=<sha>` | commit messages only, never diffs | read |
| `POST /note` (forks), `GET /notes` (prime) | a child's note; its children's notes | note |

**Rules.**

- It answers; it never writes to the Island log on a caller's behalf. Its only writes are its own `access` entries.
- A daily budget per Vessel, in calls and in bytes, set in its config. Over budget is refused, and the refusal is counted.
- `access` entries are batched: one per Vessel and kind of request, at most every 30 seconds, with the number of calls, bytes each way, and one digest over the calls' digests. The agent records each model message in its Chronicle with the same digest, which ties the two.
- Tasks are at-least-once. The agent's cursor is in its sealed state, so a rolled-back turn pulls its task again.
- The providers, the models and the budgets are a config file read at start ("Providers").

**Providers (D2).** The agent always speaks one API, OpenAI-compatible chat completions, to one place, the island socket. Which model answers is `island-socket`'s config, never the agent's:

```toml
default = "local"

[provider.local]          # a model on the server itself
base_url = "http://127.0.0.1:8080/v1"
models = ["qwen3.6-35b-a3b"]

[provider.gpu]            # Jarmo's own GPU host, as a wg-island peer
base_url = "http://10.77.0.4:8000/v1"
models = ["glm-5.3"]

[provider.hosted]         # any hosted OpenAI-compatible API
base_url = "https://api.example.com/v1"
key_file = "/etc/island-socket/hosted.key"
models = ["model-a", "model-b"]
```

- The agent names a model; the socket finds the provider that lists it, or uses `default` when the agent names none. A model no provider lists is refused.
- Keys live in files readable by `island-socket` only; the agent never sees one.
- Switching provider is a config change and a restart of `island-socket`; the agent and its Vessel are untouched.
- Each provider has its own daily budget, and the `access` entry names the provider, so the log shows which models were used and how much.
- A provider must be reachable from the server: on the server, on `wg-island`, or over the internet. The host firewall does not limit what the server sends, but UpCloud's outer firewall (planned, stateless) lets in replies only from TCP ports 443 and 80. A provider on another port over the internet needs one more rule there, written down in `docs/server.md`.
- A provider that is not natively OpenAI-compatible is reached through its own compatibility endpoint where it has one, or a small adapter in `island-socket`. Which providers need an adapter is checked when each is added, not assumed.

## Actions and the sender

An action is a Taso effect: provider, exact request bytes, a key, a consequence. `island-sender` is the only thing that acts on one. It serves every socket in the Warden's senders directory, so a restart of a Vessel needs nothing from it.

| Provider | Request bytes | Approval | How it is sent | Sent once because |
| --- | --- | --- | --- | --- |
| `flare.say` | JSON `{ task, text }`, at most 256 KiB | none: talking to Jarmo is the review | handed to `flare-server` on a root-only Unix socket | the session keeps one event per key |
| `island.verse` | canonical `{ stream, verse }` | required | a `Submission` signed with the Vessel's writer key, through the broker | the `dispatch` entry, below |

Both are declared `Irreversible`: said is said, and a log entry stays.

**The order of decisions** for a released action (`island-client`'s `approval` module), per provider:

1. Already dispatched, this op or an earlier op of the same Vessel with the same key: report applied, send nothing.
2. Approved, or needing no approval: send, then report applied. An approval counts if no dispatch has used it and no decline of the same request came after it.
3. Declined, with no approval after the decline: report not applied.
4. Otherwise: leave it waiting, **whether Taso lists it as unknown or not**. Nothing expires on its own: Jarmo declines what he does not want, and the cap of 16 waiting actions keeps the list short. An unknown barrier that waits keeps the Vessel from releasing anything new, which is right: an action whose fate is open is answered before the next.

*Changed in step 2.* There was a fourth rule: an unapproved action lost in a Warden restart was reported not applied ("lapsed"), to give the Vessel its effect authority back at once. Taso's `unknown` flag does not only mean a restart: it also marks every effect an earlier listing handed out (`effects.rs`, `hand_out`), so a sender that polls its socket sees it on every pass but the first. On the server, island-sender lapsed `flare-home`'s first action half a second after it was proposed. Only the log's dispatches say whether an action was let out; the flag decides nothing. The stand-in Warden in the tests and the sender-history harness now hand out as Taso does, and the harness catches the old rule.

**The gate holds the same rule** (built in step 1). A `dispatch` is accepted only if, checked in this order: the same Vessel has not dispatched the same key (`AlreadyDispatched`); the entry it names is an `approval` that approves the same request (`Unapproved`); no earlier dispatch used that approval (`ApprovalUsed`); no decline of the request comes after that approval (`Declined`). A refusal is a `verdict` like any other. This differs from S2 in one place: a decline no longer stops the request forever, only the approvals before it, so Jarmo can change his mind by approving again. The rule is a structural rule of the log, as grants are, not an invariant of the policy: no policy can loosen it.

**An action's key** is `blake3_derive_key("island.effect-key.v1", key)`, where `key` is the effect's idempotency key when its provider deduplicates by one, and otherwise `taso-op:<op>`, since Taso never reuses an op within a Vessel.

**Sending a verse once.** The sender first appends a `dispatch` entry naming the Vessel, the op, the action's key, the approval it consumes and the counter the Vessel's key will use. Only then does it submit the verse with that counter. A sender that dies between the two finds its `dispatch` entry next time and submits the same bytes again; the log's counter rule answers an exact resend with the first answer. A verse the gate refuses is reported not applied with the `verdict`'s position.

**The Vessel's writer key** lives with `island-sender`, in a root-only file, one per Vessel, each with its own grant (D3: `verse` on `flare`). OpenBao Transit takes it over later, with the root key.

**Two seams, empty in this slice.** A check before step 3 that can only refuse: where Stormgate's comparison against the task that started the work would sit. And the approver set of step 3 is whoever holds the `approval` right: Runo becomes an approver by a grant, not by code.

## Record formats

Four kinds join the eight of slice 1. Encoding, hashing and signatures are unchanged.

| Tag | Kind | Stream | Body |
| --- | --- | --- | --- |
| 6 | `receipt` | `repos` | `{ repo: String, branch: String, head: [u8;20], previous: Option<[u8;20]>, commits: u32, observed_at: u64 }` |
| 7 | `access` | `access` | `{ vessel: [u8;16], what: u8, provider: String, calls: u32, bytes_in: u64, bytes_out: u64, digest: [u8;32] }`; `what`: inference 0, read 1, task 2, note 3; `provider` empty unless inference |
| 10 | `approval` | `approvals` | `{ request: [u8;32], verdict: u8, proof: Option<Proof> }`; `verdict`: approve 0, decline 1; `proof` is empty until S3 fixes its form |
| 11 | `dispatch` | `approvals` | `{ vessel: [u8;16], op: u64, key: [u8;32], request: [u8;32], approval: u64, counter: u64 }` |

`request = blake3_derive_key("island.effect-request.v1", u64_le(len(provider)) || provider || request_bytes)`, as S2 has it.

**Writers and grants.** Each new writer is a key with one narrow grant, like every writer: the observer (`receipt` on `repos`), `island-socket` (`access` on `access`), `island-sender` (`dispatch` on `approvals`), each Vessel (`verse` on `flare`), and Jarmo's approving key (`approval` on `approvals`).

**New invariants,** each a tightening of the policy in force:

- `ArtefactRefHasReceipt` (tag 3): an artefact verse's refs each match a receipt's `head` or lie in a receipt's range. It replaces nothing; `ArtefactHasRef` stays.
- `ApprovalHasPasskey` (tag 4): an approval carries a proof that verifies against a registered passkey. Specified after S3; added to the policy when the phone can make one.

**The S2 stand-in ends.** S2 wrote approvals as `preference` entries on `plan`, and only on throwaway logs. Once the kinds exist, `island-cli approve` writes an `approval` and the sender reads nothing else. An approver is whoever the log's grants let write one, so the sender's `--approver` option goes.

**Replay** stays pure: a receipt, an approval and a dispatch are judged from the state alone. State gains the approvals (each with the dispatch that used it), the newest decline per request, and the (Vessel, key) pairs dispatched; its hash domain becomes `island.state.v2`. The receipts join the state in step 7, with the invariant that reads them. Note there that a receipt's `head` is a 20-byte commit id while a verse's refs are 32-byte hashes: `ArtefactRefHasReceipt` needs a stated mapping between them.

**Logs from before step 1.** `repos`, `access` and `approvals` are system streams now, holding only their own kinds. A log that already holds a verse or a preference on a stream of one of those names no longer replays. The real log has none; the S2 throwaway logs wrote their marks on `plan` and are not kept.

## Session protocol, version 2

Version 1 keeps working; a client says which it speaks when it attaches.

| New verb | Carries | Signed by |
| --- | --- | --- |
| `say` | a message for the agent | nothing in this slice: a task can only lead to actions, and actions are approved |
| `approve`, `decline` | an `approval` submission the client built | the device's key with the `approval` right |
| `stop` | ask the agent to drop the task it is on | nothing |

| New event | Payload |
| --- | --- |
| `agent_state` | idle, working (task, turn), forked (children, slots left), waiting (for approval, for Jarmo) |
| `fork_note` | child, text, done |
| `action_waiting` | vessel, op, provider, request hash, the request decoded for reading, since when |
| `action_settled` | request hash; sent (entry) or declined (entry) |
| `nudge` | repo, branch, commits since the last verse, since when |

`action_waiting` and `action_settled` are built by `flare-server` from `island-sender`'s reports and from the log. A client with a replica recomputes the request hash from the shown request and checks the settled entry in its own log, so "sent" is never taken on the server's word.

**The face.** The screens, flows, signing friction, notifications and the split between phone and laptop are Jarmo's UX plan, not this spec. What the spec fixes for it:

- It is one more client of this protocol. `flare-server` learns to serve one page from itself and to accept that one origin; nothing else changes for it.
- Every waiting thing has an event of its own and an id, so a face can list "waiting for you" without reading prose.
- Everything shown from the log can be checked on the device; the protocol always names the entry.
- The terminal gets every verb first. A build step is done when `flare-tui` can do it.

## Repos as witnesses

Carried from the slice 1 spec with one change: the agent drafts, when asked.

**The observer** is a task inside `island-socket`: with its read-only GitHub credential it polls each mapped repo's branch heads and submits a `receipt` for each new head. Hashes and counts only. The server takes no webhooks; it has no public TCP port.

**Mapping.** A `preference` on `plan`, `repo:<owner>/<name>` → stream, so it can change freely.

**Verse to repo.** With `ArtefactRefHasReceipt` in the policy, a verse about an artefact names a commit the log has a receipt for. The gate reads the receipt, never GitHub.

**Repo to verse.** `flare-server` compares receipts with verses and raises a `nudge`. On Jarmo's word the agent fetches the commit messages, drafts, and replies. The verse is his to correct and sing.

**Rules.** A nudge is a signal and blocks nothing. A receipt proves that something changed; only the verse says what it meant. Diffs go to no model in this slice. Only pushed work is seen.

## The Taso node

On the server, once, as `docs/server.md` will list step by step:

- `taso-ctl init` writes `/etc/taso/warden.json` and checks the host; the `flare-home` entry is added by hand.
- Units: `taso-blkd`, `taso-wardend` (after it), `island-socket`, `island-sender` (root, after the Warden), beside the existing `island-broker` and `flare-server`.
- `/var/lib/taso` holds the block log, mounts and images. The block log's size is set at `init`; 32 GiB to start.
- No Docker on this host: it would add firewall chains and forwarding. Taso itself opens no port and adds no rule *(ran: the firewall's fingerprint was the same after S2)*.
- The Warden is built to be killed; there is no shutdown step. A reboot is a crash it recovers from.
- A wrong `whoami` would be a wrong identity, so `island-socket` reads the same `warden.json` the Warden reads and refuses to start if two Vessels' uid ranges overlap.

## Harness and gates

**In `just test`,** deterministic as in slice 1:

- `island-model` gains the four kinds and the approval-and-dispatch rule in a few dozen lines.
- The log harness (built in step 1) generates approvals, declines and dispatches by the two writers granted them; oracle 8 checks from the stored entries alone that every accepted dispatch names an unused approval of its request with no later decline, and that no (Vessel, key) is dispatched twice. Coverage requires each of the four refusals.
- A generator of sender histories (built in step 2: `crates/island-sender/tests/history.rs`, on `island_sender::serve`, the pass the service runs): actions released, approvals and declines in any order and number, sender deaths at every step (before the `dispatch`, between it and the verse, before the report), Warden restarts that mark actions unknown, turns redone with the same keys.
- Oracles: no action is sent without an approval that names it; none is sent twice; a declined one is never sent; an approval lets out one action; after any history every barrier action is settled or waiting, never lost; the model and `island-sender`'s decisions agree.
- The agent loop runs against a scripted model (answers replayed from a file) and a stand-in Warden, so its turns, keys and restarts are tested without a GPU or Taso.

**On the server,** `just vessel-gate`: `s2-spike.sh` with `flare-agent` and the scripted model in place of the spike's agent. It keeps runs a to h and s, and adds: a task answered through the session; a fork, a note and a promotion asked for over the Field; a kill in each of them.

**Spike S1** (turmoil) has no note in the repo and is not needed here: the new parts meet over Unix sockets and are tested against stand-ins. It stays parked until a second node.

## Spikes

| Spike | Question | Box | Decides |
| --- | --- | --- | --- |
| S3 passkey | Carried from slice 1: can the server supply the WebAuthn challenge derived from the request hash, and can the gate verify the assertion from the log alone? | half a day | The form of `proof` and of `ApprovalHasPasskey` |
| S4 promotion over the Field | Fork and promote asked for by the agent, with `restart: always`: is the prime started again on the child's files? When it has written files since the fork's Seal, what does it start on? What becomes of a prime's released action when its turn never ended? How long does a Seal take with a real `/work`? How are a finished child's slot and dataset reclaimed, if at all? | 1 day | The forking steps above; how many slots to give; the turn length |
| | **S4 ran (2026-11-14, `/root/island-s2-20261114-112820`).** Started again on the child's files: yes, at the child's turn. Files since the fork's Seal: a prime that sealed after the fork cannot promote (M7); unsealed files are gone. The unended turn's released action: unknown after the restart, with the adopted ones, and no effect authority until they are settled. Seals: 0.72 s after writing 200 MiB in 4000 files, 1.5 s after rewriting 40 of them, under 0.05 s for a small turn; a fork 0.02 s; promotion to the next start 2.3 s, so the two-minute turn stands. Slots are never given again and finished children's datasets stay mounted. | | Forking steps 1–6 above; eight slots |
| S5 thinking through the socket | A streamed completion of realistic size from prime and child, through `island-socket`, to each configured provider (local, `wg-island`, hosted) | half a day | Whether streaming needs anything more than run h showed |
| S6 the Pi port | Which parts of Pi's loop are ported and which are left: sized by reading it, before any code | 1 day | The agent's milestones |

## Build order

Each step leaves something that runs, and ends with `git push origin main`.

1. **The four kinds.** Types, encoding, golden vectors, replay, the model. `island-cli approve` writes an `approval`; the sender's rule moves from the stand-in to the kinds. Harness oracles for approval and dispatch.
2. **The Taso node.** `init`, the units, `flare-home` running S2's scripted agent as a long-lived Vessel, `island-sender` as a service on the real log. Reboot and kill tests. Spike S4 here. *Built:* `scripts/node/install.sh` and `check.sh`, the units in `deploy/server/`, `docs/server.md` section 6; `island-sender --senders` serves the Warden's whole senders directory; spike S4 is run p of `s2-spike.sh`, its agent `scripts/s2/s4-agent.py`, which is also `flare-home`'s stand-in agent until step 4.
3. **`island-socket`.** `whoami`, the model pass-through against a stub, then the providers of D2 one by one (S5), budgets, `access` entries.
4. **`flare-agent`, first milestone.** The loop with file tools and `say`; tasks from the session; `flare-tui` gets `say`. First conversation with the agent.
5. **The agent's verse.** `verse`, the `island.verse` provider, `action_waiting`, `approve` and `decline` in `flare-tui`. First approved agent verse in the real log.
6. **Forks.** `fork`, notes, `promote`, the restart on the child's files; `vessel-gate` complete.
7. **Repos.** The observer, receipts, the mapping, nudges, `ArtefactRefHasReceipt` as a real tightening. The path of "The slice" works end to end in the terminal.
8. **The face,** from the UX plan, beside steps 4 to 7 as its events arrive. The passkey when S3 has run.
9. **Two weeks of real use,** then the slice 3 spec: mail and calendar as providers, Runo, Stormgate.

## Wishes for Taso

Island works on main as it is. These would make it better, in this order:

1. **A `Proposals` query**, already planned: review of a fork's exact actions before promotion, and D4 without notes.
2. **A way for an agent to learn its origin** (prime or fork, its slot): today Island answers it from the peer uid.
3. **A group for the senders directory**, or a line in the docs that a root relay is the intended shape.
4. **A command for the operator `resolve`**: today only a sender can settle an open action.
5. **Slots that free:** a fork slot usable again once its child is promoted or purged, so a long-lived Vessel can fork for as long as it lives.
6. **The inbox**: tasks then enter through Taso, and the island socket's task pull goes.
7. A line in the docs: a socket placed in a Vessel's root filesystem is reachable, and its peer uid names the Vessel.
8. **A Warden that starts after a crash of the block log** (found in step 2): `taso-blkd` killed without `quit` leaves the Vessels' filesystems mounted on deleted devices, and the Warden refuses to start while a dataset is mounted from another device than the one now serving it. Island's unit unmounts such mounts before the Warden starts (`deploy/server/taso-unmount-stale`).
9. **A sender listing that says why an effect is unknown** (found in step 2): a restart, or only handed out before (`hand_out`). Island no longer needs it (it decides on the log), but a sender that does is easy to write wrong.
