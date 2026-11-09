#!/usr/bin/env bash
# Synthetic-only exercise. Run inside a network namespace containing only lo.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
: "${CADDY_BIN:?Set CADDY_BIN to a verified Caddy executable}"
[[ -x "$CADDY_BIN" ]] || { printf '%s\n' 'Caddy executable unavailable.' >&2; exit 1; }
interfaces="$(ip -o link show | awk -F ': ' '{print $2}')"
[[ "$interfaces" == 'lo' ]] || { printf '%s\n' 'An isolated loopback-only network namespace is required.' >&2; exit 1; }
work="$(mktemp -d "${TMPDIR:-/tmp}/discord-mcp-caddy.XXXXXXXX")"
child=''
cleanup() {
    result=$?
    trap - EXIT
    if [[ -n "$child" ]]; then
        kill "$child" 2>/dev/null || true
        wait "$child" 2>/dev/null || true
    fi
    rm -rf -- "$work"
    exit "$result"
}
trap cleanup EXIT
mkdir -p "$work/home" "$work/data" "$work/config" "$work/existing"
cargo_env=(env -i PATH="$PATH" HOME="$work/home" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
    RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" TMPDIR="$work" CARGO_NET_OFFLINE=true
    CARGO_BUILD_JOBS=2 TOKIO_WORKER_THREADS=2)
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    cargo_env+=("CARGO_TARGET_DIR=$CARGO_TARGET_DIR")
fi
"${cargo_env[@]}" cargo build --locked --all-features --manifest-path "$root/Cargo.toml"
binary="${CARGO_TARGET_DIR:-$root/target}/debug/discord-mcp"
printf '%s\n' 'operator-data' > "$work/existing/sentinel"
if env -i PATH="$PATH" HOME="$work/home" "$binary" setup remote bundle example.test "$work/existing" --confirm >/dev/null 2>&1; then
    printf '%s\n' 'Existing bundle destination was not rejected.' >&2
    exit 1
fi
[[ "$(cat "$work/existing/sentinel")" == 'operator-data' ]] || { printf '%s\n' 'Existing content changed.' >&2; exit 1; }
env -i PATH="$PATH" HOME="$work/home" "$binary" setup remote bundle example.test "$work/bundle" --confirm
cat "$root/drills/caddy/global-options.caddy" "$work/bundle/discord-mcp.caddy" > "$work/Caddyfile"
env -i PATH="$PATH" HOME="$work/home" XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data" \
    "$CADDY_BIN" run --config "$work/Caddyfile" --adapter caddyfile > "$work/caddy.log" 2>&1 &
child=$!
ca="$work/data/caddy/pki/authorities/local/root.crt"
for _ in {1..100}; do
    [[ -s "$ca" ]] && break
    kill -0 "$child" 2>/dev/null || { printf '%s\n' 'Caddy fixture exited before readiness.' >&2; exit 1; }
    sleep 0.1
done
[[ -s "$ca" ]] || { printf '%s\n' 'Synthetic CA was not created.' >&2; exit 1; }
status="$(curl --silent --retry 8 --retry-all-errors --retry-delay 1 --cacert "$ca" \
    --resolve example.test:8443:127.0.0.1 --noproxy '*' \
    --header 'Authorization: Bearer synthetic-log-marker' --output /dev/null \
    --write-out '%{http_code}' 'https://example.test:8443/mcp?synthetic-log-marker=1')"
[[ "$status" == '502' ]] || { printf '%s\n' 'Expected backend outage response was not observed.' >&2; exit 1; }
"${cargo_env[@]}" DISCORD_MCP_CADDY_CA="$ca" DISCORD_MCP_CADDY_CONFIG="$work/Caddyfile" \
    cargo test --locked --all-features --manifest-path "$root/Cargo.toml" \
    --test caddy_tls -- --ignored --test-threads=1
"${cargo_env[@]}" CIPHERA_TEST_PROXY_CA="$ca" \
    DISCORD_MCP_BUNDLE_PATH="$work/bundle" cargo test --locked --all-features \
    --manifest-path "$root/Cargo.toml" --test ciphera_cli -- --test-threads=1
if grep -q 'synthetic-log-marker' "$work/caddy.log"; then
    printf '%s\n' 'Synthetic marker was found in Caddy logs.' >&2
    exit 1
fi
printf '%s\n' 'Verified bundle preservation, synthetic injection contract, HTTPS, authentication, outage and absence of the synthetic marker in Caddy logs.'
