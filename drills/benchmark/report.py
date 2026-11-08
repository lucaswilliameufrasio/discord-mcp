#!/usr/bin/env python3
"""Run the synthetic benchmark and emit an allowlisted comparison report."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def source_digest(root):
    """Hash explicitly scoped build inputs, never emit paths or file contents."""
    paths = [root / "Cargo.toml", root / "Cargo.lock", root / "examples/mcp_benchmark.rs"]
    paths.extend((root / "src").rglob("*.rs"))
    digest = hashlib.sha256()
    for path in sorted(paths):
        if path.is_symlink() or not path.is_file():
            raise ValueError("missing or symlinked source input")
        relative = path.relative_to(root).as_posix().encode()
        content = path.read_bytes()
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def run_capture(args, env):
    return subprocess.run(args, cwd=ROOT, env=env, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, timeout=600).stdout.strip()


def main():
    # Only explicitly named process inputs are inherited. Never serialize them.
    env = {name: os.environ[name] for name in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR") if name in os.environ}
    env["CARGO_NET_OFFLINE"] = "true"
    env["CARGO_BUILD_JOBS"] = "2"
    before = source_digest(ROOT)
    revision = run_capture(["git", "rev-parse", "HEAD"], env)
    if not re.fullmatch(r"[0-9a-f]{40,64}", revision):
        raise ValueError("invalid revision")
    dirty = bool(run_capture(["git", "status", "--porcelain", "--untracked-files=normal"], env))
    rustc = run_capture(["rustc", "--version"], env)
    cargo = run_capture(["cargo", "--version"], env)
    # Validate tool output to prevent arbitrary configuration or path disclosure.
    if not re.fullmatch(r"rustc [0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.]+)?(?: \([0-9a-f]+ [0-9-]+\))?", rustc):
        raise ValueError("unexpected rustc version")
    if not re.fullmatch(r"cargo [0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.]+)?(?: \([0-9a-f]+ [0-9-]+\))?", cargo):
        raise ValueError("unexpected cargo version")
    result = json.loads(run_capture(["cargo", "run", "--offline", "--locked", "--release", "--features", "benchmark-fixture", "--example", "mcp_benchmark"], env))
    if result.get("schema") != 3 or source_digest(ROOT) != before:
        raise ValueError("benchmark schema or source changed during run")
    kernel = re.match(r"^[0-9]+\.[0-9]+\.[0-9]+", platform.release())
    print(json.dumps({"report_schema": 1, "timestamp_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                      "provenance": {"revision": revision, "working_tree_dirty": dirty,
                                     "source_sha256": before, "rustc": rustc, "cargo": cargo,
                                     "kernel_version": kernel.group(0) if kernel else None, "build": "release", "feature": "benchmark-fixture"},
                      "benchmark": result}, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError, json.JSONDecodeError):
        print("Benchmark report failed; no report emitted. Check prerequisites and retry.", file=sys.stderr)
        sys.exit(1)
