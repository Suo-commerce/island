# Island workspace tasks.  There is no hosted CI: `just check test` is the
# gate, run on a laptop or on the Island server before a push.

# Seeds per harness run: `just SEEDS=500 harness`.
SEEDS := "10000"

# Where `just init` starts a log: `just LOG=/var/lib/island/island.log init`.
LOG := ".island/island.log"
ROOT_KEY := ".island/root.key"
APPENDER_KEY := ".island/appender.key"

# Where `just broker` listens.
LISTEN := "127.0.0.1:4747"

# List the recipes.
default:
    @just --list

# Everything to check before the tests: format, Clippy with the bans, dependency rules.
check: fmt lint deps

# Formatting, check only.
fmt:
    cargo fmt --all --check

# Clippy with the determinism bans from clippy.toml; warnings are errors.
lint:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Dependency rules, licences, advisories and sources.
deps:
    cargo deny check
    scripts/check-deps.py

# Unit and component tests.
test:
    cargo test --workspace --all-features --locked

# The seeded harness, optimized: archived failures first, then SEEDS new seeds.
harness:
    ISLAND_HARNESS_SEEDS={{SEEDS}} cargo test -p island-harness --release --locked --test harness -- archived_seeds_pass
    ISLAND_HARNESS_SEEDS={{SEEDS}} cargo test -p island-harness --release --locked --test harness

# Genesis: start a new log with entries 0, 1 and 2 (the first policy). Key files come from `island-cli keygen`.
init:
    cargo run --quiet --locked -p island-cli -- init --log {{LOG}} --root-key {{ROOT_KEY}} --appender-key {{APPENDER_KEY}}

# Build the programs, optimized, into target/release.
build:
    cargo build --release --locked -p island-cli -p island-broker -p flare-link -p flare-server -p flare-tui

# Build the programs and put them in ~/.local/bin, where the user services run them from.
install: build
    install -d ~/.local/bin
    install -m 755 target/release/island-cli target/release/island-broker target/release/flare-link target/release/flare-server target/release/flare-tui ~/.local/bin/

# Serve the log: the MCP tools at /mcp, GET /log and GET /head. Stop it with Ctrl-C.
broker:
    cargo build --locked -p island-broker -p island-cli
    target/debug/island-broker --log {{LOG}} --appender-key {{APPENDER_KEY}} --root $(target/debug/island-cli public {{ROOT_KEY}}) --listen {{LISTEN}}

# The shared Flare session server, beside a running broker. Stop it with Ctrl-C.
session:
    cargo build --locked -p flare-server -p island-cli
    target/debug/flare-server --broker http://{{LISTEN}} --root $(target/debug/island-cli public {{ROOT_KEY}})

# A terminal on the shared session, signing verses with KEY. `just KEY=.island/daily.key tui`
KEY := ".island/daily.key"
tui:
    cargo build --locked -p flare-tui -p island-cli
    target/debug/flare-tui --client $(hostname) --key {{KEY}} --broker http://{{LISTEN}} --root $(target/debug/island-cli public {{ROOT_KEY}})

# Spike S2: the agent and sender scripts against Taso's codec and a stand-in Warden (no root needed).
s2-test:
    python3 -m unittest scripts/s2/test_s2_field.py scripts/s2/test_s2_flow.py

# Spike S2: island-sender against a real Island log and broker, with a stand-in Warden (no root needed).
s2-sender-test:
    cargo build --locked -p island-cli -p island-broker -p island-sender
    python3 -m unittest scripts/s2/test_sender_local.py

# Spike S2 on the server, as root: Island's agent in Taso Vessels (runs a to h, and s: island-sender). TASO names the Taso checkout.
s2-spike TASO="/root/taso":
    TASO={{TASO}} scripts/s2-spike.sh
