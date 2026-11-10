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

# Build the programs, optimized: target/release/island-cli and target/release/island-broker.
build:
    cargo build --release --locked -p island-cli -p island-broker

# Serve the log: the MCP tools at /mcp, GET /log and GET /head. Stop it with Ctrl-C.
broker:
    cargo run --quiet --locked -p island-broker -- --log {{LOG}} --appender-key {{APPENDER_KEY}} --root $(cargo run --quiet --locked -p island-cli -- public {{ROOT_KEY}}) --listen {{LISTEN}}
