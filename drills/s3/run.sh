#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="$root/drills/s3/Cargo.toml"
image='floci/floci:2.3.0@sha256:e0b292df4dc9559b7b1bf4f51bdda4bb8e269276236438cd03cae661260ea8a9'
docker=(docker --context rootless)
security="$("${docker[@]}" info --format '{{json .SecurityOptions}}')"
[[ "$security" == *name=rootless* ]] || { printf '%s\n' 'Rootless daemon required.' >&2; exit 1; }
"${docker[@]}" image inspect "$image" --format '{{.Id}}' >/dev/null
cargo build --locked --manifest-path "$manifest" --target-dir "$root/target/s3-drill"
cargo build --locked --manifest-path "$root/Cargo.toml" --features encrypted-s3-backup --bin discord-mcp --target-dir "$root/target/s3-drill"
container=''
cleanup() {
    result=$?
    trap - EXIT
    if [[ -n "$container" ]]; then
        if ! "${docker[@]}" stop --time 5 "$container" >/dev/null; then
            printf '%s\n' 'Could not stop the drill-owned container; inspect it manually.' >&2
            result=1
        fi
    fi
    exit "$result"
}
trap cleanup EXIT
container="$("${docker[@]}" run --detach --rm --pull never --publish '127.0.0.1::4566' \
    --env FLOCI_STORAGE_MODE=memory --env FLOCI_DEFAULT_REGION=us-east-1 "$image")"
address="$("${docker[@]}" port "$container" 4566/tcp)"
[[ "$address" =~ ^127\.0\.0\.1:[0-9]+$ ]] || { printf '%s\n' 'Unexpected fixture address.' >&2; exit 1; }
python3 - "$address" <<'PY'
import http.client, sys, time
host, port = sys.argv[1].split(':')
deadline = time.monotonic() + 60
while time.monotonic() < deadline:
    try:
        conn = http.client.HTTPConnection(host, int(port), timeout=1)
        conn.request('GET', '/_floci/health')
        response = conn.getresponse()
        ready = response.status == 200 and bool(response.read())
        conn.close()
        if ready:
            break
    except (OSError, http.client.HTTPException):
        pass
    time.sleep(0.2)
else:
    raise SystemExit('Fixture did not become ready')
PY
timeout 120s env -i PATH="$PATH" HOME="$HOME" TMPDIR="${TMPDIR:-/tmp/opencode}" \
    DISCORD_MCP_DRILL_BINARY="$root/target/s3-drill/debug/discord-mcp" \
    "$root/target/s3-drill/debug/discord-mcp-s3-drill" "$address"
timeout 180s env -i PATH="$PATH" HOME="$HOME" TMPDIR="${TMPDIR:-/tmp/opencode}" \
    CARGO_NET_OFFLINE=true DISCORD_MCP_FLOCI_FIXTURE_ADDRESS="$address" \
    cargo test --locked --manifest-path "$root/Cargo.toml" --features encrypted-s3-backup \
    --test remote_cli mediated_backup_requires_consent_and_restores_from_floci -- --ignored --nocapture
timeout 180s env -i PATH="$PATH" HOME="$HOME" TMPDIR="${TMPDIR:-/tmp/opencode}" \
    CARGO_NET_OFFLINE=true DISCORD_MCP_FLOCI_FIXTURE_ADDRESS="$address" \
    cargo test --locked --manifest-path "$root/Cargo.toml" --features encrypted-s3-backup \
    --test remote_cli lost_put_response_authorized_comparison_with_floci -- --ignored --nocapture
