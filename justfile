# Island workspace tasks.  CI runs `just check` and `just test`.

# Seeds per harness run: `just SEEDS=500 harness`.
SEEDS := "10000"

# List the recipes.
default:
    @just --list

# Everything CI checks before the tests: format, Clippy with the bans, dependency rules.
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

# The seeded harness (generators and oracles arrive with build step 4).
harness:
    ISLAND_HARNESS_SEEDS={{SEEDS}} cargo test -p island-harness --release --locked

# Genesis: writes entries 0 and 1 (arrives with island-cli in build step 3).
init:
    @echo "just init: island-cli init arrives with build step 3 (docs/spec.md, Build order)" >&2
    @exit 1
