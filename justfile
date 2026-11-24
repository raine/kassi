# Rust project checks

set positional-arguments
set shell := ["bash", "-euo", "pipefail", "-c"]

pi_target := "aarch64-unknown-linux-musl"

# List available commands
default:
    @just --list

# Run the fast local read-only check set
check: check-fast-readonly clippy

# Run cheap read-only checks in parallel
check-fast-readonly:
    @checkle run fast-readonly

# Run commit-time checks without mutating files
pre-commit: check-fast-readonly clippy

# Run every check, including tests
check-full: check test

# Run the full gate and fail if there are uncommitted changes
check-ci: check-full
    #!/usr/bin/env bash
    set -euo pipefail
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "Error: check caused uncommitted changes"
        git diff --stat
        exit 1
    fi

# Configure Git to use the repository hooks
install-hooks:
    git config core.hooksPath hooks

# Install local tools used by quality gates
install-quality-tools:
    cargo install checkle cargo-machete cargo-nextest cargo-deny
    cargo install cargo-zigbuild --version ">=0.24" # older versions reject a linker flag rustc passes for aarch64

# Check dependency advisories and licenses
deny:
    @checkle run cargo-deny

# Format Rust files
format:
    @scripts/quiet-check format cargo fmt --all

# Check Rust formatting without changing files
format-check:
    @checkle run format-check

# Run clippy and fail on any warnings
clippy:
    @checkle run clippy

# Auto-fix clippy warnings
clippy-fix:
    @scripts/quiet-check clippy-fix cargo clippy --fix --allow-dirty --target-dir target/clippy --all-targets -- -D warnings -W clippy::all

# Build the project
build:
    @scripts/quiet-check build cargo build --locked

# Run all tests
test:
    @checkle run tests

# Run tests matching a filter, e.g. `just test-filter auth::`
test-filter filter:
    cargo nextest run --target-dir target/test --locked "$1"

# Install kassi into ~/.cargo/bin
install:
    cargo install --locked --path .

# Build a static Raspberry Pi (64-bit) binary with cargo-zigbuild
build-pi:
    cargo zigbuild --locked --release --target {{pi_target}}
    @echo "target/{{pi_target}}/release/kassi"

# Build for the Pi and install the binary as HOST:~/.local/bin/kassi
deploy-pi host: build-pi
    ssh "$1" 'mkdir -p ~/.local/bin'
    scp "target/{{pi_target}}/release/kassi" "$1:.local/bin/kassi.new"
    @# A rename replaces the binary even while kassi is running from it.
    ssh "$1" 'mv ~/.local/bin/kassi.new ~/.local/bin/kassi'
