#!/usr/bin/env bash
# Synthetic-only tests; an isolated in-memory Floci fixture must already be running.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
: "${FLOCI_ADDRESS:?Set FLOCI_ADDRESS to the dedicated synthetic loopback fixture}"
if [[ ! "$FLOCI_ADDRESS" =~ ^127\.0\.0\.1:([0-9]{1,5})$ ]]; then
    printf '%s\n' 'Only an IPv4 loopback fixture address is allowed.' >&2
    exit 1
fi
port="${BASH_REMATCH[1]}"
if (( 10#$port < 1 || 10#$port > 65535 )); then
    printf '%s\n' 'Invalid fixture port.' >&2
    exit 1
fi
interfaces="$(ip -o link show | awk -F ': ' '{print $2}')"
[[ "$interfaces" == 'lo' ]] || { printf '%s\n' 'An isolated loopback-only network namespace is required.' >&2; exit 1; }
work="$(mktemp -d "${TMPDIR:-/tmp}/discord-mcp-floci.XXXXXXXX")"
cleanup() {
    result=$?
    trap - EXIT
    rm -rf -- "$work"
    exit "$result"
}
trap cleanup EXIT
mkdir -p "$work/home"
cargo_env=(env -i PATH="$PATH" HOME="$work/home" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
    RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" TMPDIR="$work" CARGO_NET_OFFLINE=true
    CARGO_BUILD_JOBS=2 TOKIO_WORKER_THREADS=2 DISCORD_MCP_FLOCI_FIXTURE_ADDRESS="$FLOCI_ADDRESS")
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    cargo_env+=("CARGO_TARGET_DIR=$CARGO_TARGET_DIR")
fi
"${cargo_env[@]}" cargo test --locked --all-features --manifest-path "$root/Cargo.toml" \
    --test remote_cli -- --ignored --test-threads=1
printf '%s\n' 'Synthetic mediated backup, isolated restore and lost-response comparison passed.'
