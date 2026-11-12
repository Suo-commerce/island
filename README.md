# Island

Island is a signed, hash-chained log with a gate in front of it: the place
where verses about projects and work are written, judged against a policy
that can only be tightened, and replicated for verification. Flare is its
personal console. Both live in this one Cargo workspace.

The specification is [`docs/spec.md`](docs/spec.md). The repo is at build
step 9 of its "Build order":

- `island-core` holds the record formats, with golden vectors in
  `crates/island-core/tests/golden/`.
- `island-env` holds the clock, randomness, storage and key files.
- `island-auth` holds authority: grants, narrowing, revoking, and the two
  protected grants every log starts with.
- `island-gate` holds the gate: `judge`, the three invariants, and
  `strengthens`, the rule that a policy only tightens.
- `island-log` holds the appender, the reader and replay. A write the gate
  refuses leaves a `verdict` entry holding the whole submission, and replay
  judges every one of them again.
- `island-broker` serves one log: the MCP tools `append`, `read_stream`
  and `my_capabilities` at `/mcp`, and `GET /log` and `GET /head` for
  replicas. One thread owns the appender and answers every request in
  order.
- `island-client` is the other side: it builds and signs submissions,
  holds the wire format (pinned in `crates/island-client/tests/golden/`),
  and calls a broker.
- `island-cli` starts, verifies and reads a log, and writes to it, either
  in the file directly or through a broker. Its `mcp` command is the
  helper a coding session writes verses through.
- `island-model` is the reference model: what the write path must answer,
  written to be read.
- `island-harness` runs generated cases through the real appender on
  simulated storage, with the model beside it, and checks its seven
  oracles after every operation.

- `flare-core` is what Flare shows of a verified log: `see`.
- `flare-link` keeps a verified replica of the log on this machine,
  pulled from a broker; every record is checked from genesis against the
  pinned root key, and a batch with one bad record is refused whole.

- `flare-server` holds the shared sessions: append-only lists of events
  that terminals attach to over WebSocket. It holds no writer key; it
  relays signed proposals to the broker and answers `see` from its own
  verified replica.
- `flare-tui` is a terminal on a session: commands in, every event of
  the session out, verses signed with this device's key, and what `see`
  shows checked against this machine's replica.

Slice 1's last step is a week of real use (step 10).

## Layout

| Path | Holds |
| --- | --- |
| `crates/island-*` | The log, gate, authority, broker, client, CLI, reference model and harness |
| `crates/flare-*` | Flare's core, replica link, session server and terminal client |
| `docs/spec.md` | The technical spec for slice 1 |
| `harness/seeds/` | The archive of every seed that ever failed the harness |
| `clippy.toml` | The determinism bans |
| `scripts/check-deps.py`, `deny.toml` | The dependency rules; licences, advisories and sources |

## Rules `just check` enforces

- No `island-*` crate depends on a `flare-*` crate.
- `flare-*` crates reach Island only through `island-core`, `island-log` and
  `island-client`.
- `island-gate` and `island-model` depend only on `island-core`.
- Outside `island-env`, no crate reads the clock, draws randomness, starts a
  thread or uses `HashMap`/`HashSet`. Unsafe code is forbidden everywhere.

## Working in the repo

Needs [just](https://github.com/casey/just) and
[cargo-deny](https://github.com/EmbarkStudios/cargo-deny); the Rust
toolchain is pinned in `rust-toolchain.toml`.

There is no hosted CI. The checks and tests run on a laptop or on the
Island server, with the same commands; run both before a push:

```sh
just check              # format, Clippy with the bans, dependency rules
just test               # unit and component tests
just harness            # archived failures, then 10,000 seeds, optimized
just SEEDS=500 harness  # the same with another number of seeds
```

`just test` includes a short harness run (256 seeds). Each seed is one
generated case, run twice with the two traces compared byte for byte. A
failing case is shrunk and written to `harness/seeds/`; commit it with the
fix. The run also checks ten generated pairs of policies per seed: where
one strengthens the other, it accepts nothing the other refuses.

### A log for development

Under `.island/`, which git ignores. Every block below can be pasted as it
is; they share these names:

```sh
LOG=.island/island.log
APPENDER=.island/appender.key
ROOT_KEY=.island/root.key
```

Start a log. `init` writes entry 0 (genesis), entry 1 (the first grants)
and entry 2 (the first policy: a verse names its evidence, and its text is
at most 4096 bytes):

```sh
mkdir -p .island
cargo run -q -p island-cli -- keygen $ROOT_KEY
cargo run -q -p island-cli -- keygen $APPENDER
just init
ROOT=$(cargo run -q -p island-cli -- public $ROOT_KEY)
cargo run -q -p island-cli -- verify --log $LOG --root $ROOT
```

Grants are written by the root key, and a grant's id is the entry that
holds it. A key for daily writing, with a grant:

```sh
cargo run -q -p island-cli -- keygen .island/daily.key
DAILY=$(cargo run -q -p island-cli -- public .island/daily.key)
cargo run -q -p island-cli -- grant --log $LOG --appender-key $APPENDER \
    --root-key $ROOT_KEY --holder $DAILY --scope island,plan --rights verse,preference
cargo run -q -p island-cli -- grants --log $LOG --root $ROOT
```

`revoke --grant 3.0` (with the same `--log`, `--appender-key` and
`--root-key`) ends the grant at entry 3; `grant --narrows 3.0` replaces it
with a smaller one.

Write verses. The gate judges each one; a refusal exits with code 1,
leaves a `verdict` entry, and uses the writer's counter:

```sh
AS_DAILY="--log $LOG --appender-key $APPENDER --root $ROOT --key .island/daily.key"
cargo run -q -p island-cli -- verse $AS_DAILY --stream island \
    --evidence decision --text "the gate judges every verse"
cargo run -q -p island-cli -- verse $AS_DAILY --stream island \
    --evidence none --text "trust me"
cargo run -q -p island-cli -- tail --log $LOG --root $ROOT
```

Tighten the policy. The new policy is the one in force with the changes
asked for, and records the git commit of the build that judges. A change
that would loosen it is refused by the gate and recorded as a verdict:

```sh
AS_ROOT="--log $LOG --appender-key $APPENDER --root-key $ROOT_KEY"
cargo run -q -p island-cli -- policy $AS_ROOT --verse-text-max 2000 --artefact-has-ref
cargo run -q -p island-cli -- policy $AS_ROOT --verse-text-max 4096
```

Ask for a refused submission to be let through. Nothing is let through:
the request is a `preference` entry with key `override-request` on the
`plan` stream, and `replay` counts them. Name the submission by the hash
the refusal printed, or by the eight digits `tail` shows on the verdict:

```sh
REFUSED=$(cargo run -q -p island-cli -- tail --log $LOG --root $ROOT -n 100 \
    | sed -n 's/.*refused verse from [0-9a-f]*, submission \([0-9a-f]*\):.*/\1/p' | tail -n 1)
cargo run -q -p island-cli -- override $AS_DAILY $REFUSED "the evidence is in the text"
cargo run -q -p island-cli -- replay --log $LOG --root $ROOT
```

### Running the broker

The broker serves one log and holds its lock, so while it runs every
write goes through it, root's too. It listens on loopback unless told
otherwise, and answers only to the names it is given: a web page cannot
reach it through a name of its own.

```sh
just broker
```

In another terminal, the same commands as above with `--broker` in place
of `--log` and `--appender-key`. They pull the log and verify it from
genesis against the root key before they use it:

```sh
BROKER=http://127.0.0.1:4747
ROOT=$(cargo run -q -p island-cli -- public .island/root.key)
VIA_DAILY="--broker $BROKER --root $ROOT --key .island/daily.key"
cargo run -q -p island-cli -- verse $VIA_DAILY --stream island \
    --evidence decision --text "written through the broker"
cargo run -q -p island-cli -- grants --broker $BROKER --root $ROOT
cargo run -q -p island-cli -- tail --broker $BROKER --root $ROOT
curl -s $BROKER/head
```

A `policy` written through a broker names the broker's build as its
evaluator, since that is the build that judges. To reach the broker by
another name than loopback, on WireGuard for example, start it with
`--listen` and one `--allow-host` per name (`cargo run -p island-broker --
--help`). Stop it with Ctrl-C; a restart loses nothing, and clients carry
on without connecting anew.

### Verses from a coding session

A coding session writes verses through a helper, `island-cli mcp`. The
helper holds one writer key and gives the session three tools:
`append_verse`, `read_stream` and `my_capabilities`. The session never
sees the key and needs no way to the broker itself, so the helper runs
outside the session's sandbox. Each installation gets its own key and its
own grant, so the log shows where a verse came from and one installation
can be revoked alone.

A key and a grant for the installation, and the programs built once:

```sh
just build
cargo run -q -p island-cli -- keygen .island/opencode.key
SESSION=$(cargo run -q -p island-cli -- public .island/opencode.key)
ROOT=$(cargo run -q -p island-cli -- public .island/root.key)
cargo run -q -p island-cli -- grant --broker http://127.0.0.1:4747 \
    --root-key .island/root.key --holder $SESSION --scope island --rights verse
```

Then one of two ways to load the helper. If the harness can start a
program outside its sandbox, let it start the helper (MCP over standard
input and output):

```sh
HELPER="$PWD/target/release/island-cli mcp --broker http://127.0.0.1:4747 --key $PWD/.island/opencode.key --root $ROOT"
opencode mcp add island -- $HELPER
claude mcp add --scope user island -- $HELPER
```

If the harness itself runs inside the sandbox, start the helper beside
it and give the harness its address (MCP over HTTP). The helper answers
on loopback only, unless it is given `--allow-host` names:

```sh
target/release/island-cli mcp --broker http://127.0.0.1:4747 \
    --key .island/opencode.key --root $ROOT --listen 127.0.0.1:4748
```

```sh
opencode mcp add island --url http://127.0.0.1:4748/mcp
claude mcp add --scope user --transport http island http://127.0.0.1:4748/mcp
```

Whoever can reach the helper's address can write verses with its key, so
keep it on loopback or inside the sandbox's own network. If the broker
does not answer, the helper says so; a verse sent again after a lost
answer is never written twice.

### The replica, and see

`flare-link` pulls the log from a broker into a file of its own,
`~/.local/share/flare/island.log` unless told otherwise, and verifies
every record before it keeps it. `see` pulls first, then shows the
replica: an overview, or one stream.

```sh
ROOT=$(cargo run -q -p island-cli -- public .island/root.key)
cargo run -q -p flare-link -- pull --broker http://127.0.0.1:4747 --root $ROOT
cargo run -q -p flare-link -- see --broker http://127.0.0.1:4747 --root $ROOT
cargo run -q -p flare-link -- see --broker http://127.0.0.1:4747 --root $ROOT island -n 5
```

If the broker serves anything that does not verify, nothing of it is
kept: `pull` and `see` say `REFUSED` and which entry, and exit with 1.
If the broker is not running, `see` shows the replica as it stands.

### The shared session

`flare-server` keeps the sessions; every terminal attached to one sees
every event in it, whoever caused it. With the broker running, in one
terminal:

```sh
just session
```

and in another, as many times as you like, each a terminal on the same
session:

```sh
just tui
```

Then type `help`. `see` and `see island 5` show the log; `verse decision
the gate judges every verse` proposes a verse, signed with the daily key
(`just KEY=.island/opencode.key tui` signs with another). `quit` leaves;
the session goes on, and `flare-tui --after 0` shows it from the start.
A `see` is checked against this machine's replica: `✓ matches your
replica` or, if the server showed something else, `✗ DOES NOT MATCH`.

The server holds no key and answers no web page: a browser that tries
to attach is refused.

### Running them as user services

The broker, the helper, the replica and the session server can run as
user services, so they are up whenever you are logged in. The programs go to
`~/.local/bin`, and the settings to `~/.config/island/island.env`:

```sh
just install
install -d ~/.config/island ~/.config/systemd/user
ROOT=$(~/.local/bin/island-cli public .island/root.key)
cat > ~/.config/island/island.env <<END
ISLAND_LOG=$PWD/.island/island.log
ISLAND_APPENDER_KEY=$PWD/.island/appender.key
ISLAND_SESSION_KEY=$PWD/.island/opencode.key
ISLAND_ROOT=$ROOT
ISLAND_BROKER=http://127.0.0.1:4747
END
chmod 600 ~/.config/island/island.env
install -m 644 deploy/user/*.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now island-broker island-helper flare-link flare-server
systemctl --user status island-broker island-helper flare-link flare-server --no-pager
```

Stop any broker or helper you started by hand first: the log is held by
one broker at a time. The helper's terminal output, one line per tool
call, is then in `journalctl --user -u island-helper -f`.

### On the server

`docs/server.md` moves the broker and the session server to the Island
server: a WireGuard network of your own devices, the programs on its
private address only, and a firewall that leaves only WireGuard open, with
their files in `deploy/server/`.

### Signing as root without a key file

Key files are for development and for the appender. A root key that lives
in a vault signs by hand: give `--root $ROOT` instead of `--root-key`, and
the command prints the hash to sign and writes nothing; run it again with
one `--signature` per hash. The signature is verified before anything is
written. With OpenBao Transit the hash goes in as `input` to the key's
`transit/sign` path with `prehashed` left off, and the `vault:v1:...`
answer is accepted as it is. `init` asks for three signatures, every
other command for one.

Changes to Taso go to the Taso repo as pull requests, never as a fork here.
