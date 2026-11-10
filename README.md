# Island

Island is a signed, hash-chained log with a gate in front of it: the place
where verses about projects and work are written, judged against a policy
that can only be tightened, and replicated for verification. Flare is its
personal console. Both live in this one Cargo workspace.

The specification is [`docs/spec.md`](docs/spec.md). The repo is at build
step 3 of its "Build order":

- `island-core` holds the record formats, with golden vectors in
  `crates/island-core/tests/golden/`.
- `island-env` holds the clock, randomness and storage.
- `island-log` holds the appender, the reader and replay.
- `island-cli` starts, verifies and reads a log.

Grants (step 5) and the gate (step 6) are not checked yet: the appender
takes any signed, correctly counted submission on the right stream. The
other crates are still empty libraries.

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
just SEEDS=500 harness  # the seeded harness (from build step 4)
```

A log for development, under `.island/` (ignored by git):

```sh
mkdir .island
cargo run -p island-cli -- keygen .island/root.key
cargo run -p island-cli -- keygen .island/appender.key
just init               # entries 0 and 1; prints the root key to pin
cargo run -p island-cli -- verify --log .island/island.log --root <root key>
```

Key files are for development and for the appender. The root key belongs
in AgentVault; signing from there arrives with build step 5.

Changes to Taso go to the Taso repo as pull requests, never as a fork here.
