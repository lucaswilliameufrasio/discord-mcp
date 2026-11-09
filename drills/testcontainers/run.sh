#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ -n "${TESTCONTAINERS_PYTHON:-}" ]]; then
    exec "$TESTCONTAINERS_PYTHON" "$root/drills/testcontainers/run.py"
fi
parent="$root/target"
created_parent=0
if [[ ! -e "$parent" ]]; then
    mkdir "$parent"
    created_parent=1
fi
venv="$(mktemp -d "$parent/testcontainers-venv.XXXXXXXX")"
cleanup() {
    result=$?
    trap - EXIT
    rm -rf -- "$venv"
    if (( created_parent )); then
        rmdir -- "$parent" 2>/dev/null || true
    fi
    exit "$result"
}
trap cleanup EXIT

# Do not silently install or change the user's mise toolchains.
mise where python@3.13.15 >/dev/null
mise exec python@3.13.15 -- python -m venv "$venv"
"$venv/bin/python" -m pip install --no-cache-dir -r "$root/drills/testcontainers/requirements.txt"
"$venv/bin/python" "$root/drills/testcontainers/run.py"
