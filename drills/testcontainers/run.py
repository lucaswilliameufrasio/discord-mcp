#!/usr/bin/env python3
"""Run synthetic integration drills in disposable, network-isolated containers."""
import json
import io
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import tarfile
import time
import uuid

# Cleanup is owned by context managers and the signal handler below. Do not
# start Ryuk: it requires additional Docker privileges on rootless hosts.
os.environ["TESTCONTAINERS_RYUK_DISABLED"] = "true"

from testcontainers.core.container import DockerContainer
from testcontainers.core.docker_client import DockerClient


ROOT = Path(__file__).resolve().parents[2]
FLOCI = "floci/floci@sha256:e0b292df4dc9559b7b1bf4f51bdda4bb8e269276236438cd03cae661260ea8a9"
CADDY = "caddy@sha256:e70b164da81ef698b3ad19af799ea6086ce6d9e48ed4079eb9fe26f6fbf14ee5"
RUNNER = "postgres@sha256:edb0c08dc33befd0262ab3b5cf1c4f3f8cfd8b09e88c05b45adee29d6761ddb6"
REPLICATION_REV = "5d0799c419f0325da8b7d999f7d67dc0bc1255ec"
RUN_ID = uuid.uuid4().hex
LABEL = "org.discord-mcp.test-run"
ROOTLESS_CONFIGURED = False
BUILD_PARENT_CREATED = False


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def interrupted(signum, _frame):
    raise InterruptedError(f"interrupted by signal {signum}")


def runner(network, target, work):
    # The tests embed the absolute path of the compiled application binary.
    # Mount only this run's target and fixture directories, never Docker's socket.
    # Use only the image's Debian userspace; its database entrypoint is disabled.
    container = DockerContainer(RUNNER, command="600")
    container.with_kwargs(entrypoint="/bin/sleep", network_mode=network, labels={LABEL: RUN_ID}, read_only=True, cap_drop=["ALL"],
                          security_opt=["no-new-privileges"])
    container.with_tmpfs_mount("/tmp", "rw,nosuid,nodev,size=512m")
    container.with_volume_mapping(str(target), str(target), mode="ro")
    container.with_volume_mapping(str(work), "/fixture", mode="ro")
    return container


def checked_exec(container, command):
    result = container.exec(command)
    if result.exit_code != 0:
        raise RuntimeError(f"fixture test failed with exit code {result.exit_code}:\n{result.output.decode(errors='replace')}")
    print(result.output.decode(errors="replace"), end="")


def main():
    global ROOTLESS_CONFIGURED, BUILD_PARENT_CREATED
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, interrupted)
    # Resolve only the requested context; never switch the user's default.
    info = json.loads(run("docker", "--context", "rootless", "info", "--format", "{{json .SecurityOptions}}", capture_output=True, text=True).stdout)
    if not any("rootless" in item for item in info):
        raise RuntimeError("rootless Docker is required")
    context = json.loads(run("docker", "context", "inspect", "rootless", capture_output=True, text=True).stdout)
    os.environ["DOCKER_HOST"] = context[0]["Endpoints"]["docker"]["Host"]
    ROOTLESS_CONFIGURED = True
    sibling = ROOT.parent / "sqlite-replication"
    run("git", "-C", str(sibling), "cat-file", "-e", REPLICATION_REV + "^{commit}")
    client = DockerClient().client
    try:
        for image in (FLOCI, CADDY, RUNNER):
            client.images.get(image)  # Fail rather than silently downloading images.
    finally:
        client.close()
    build_parent = ROOT / "target"
    try:
        build_parent.mkdir()
        BUILD_PARENT_CREATED = True
    except FileExistsError:
        if not build_parent.is_dir():
            raise
    with tempfile.TemporaryDirectory(prefix="testcontainers-drill-", dir=build_parent) as directory:
        work = Path(directory)
        target = work / "target"
        (work / "tmp").mkdir()
        environment = {key: os.environ[key] for key in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN") if key in os.environ}
        environment.update(TMPDIR=str(work / "tmp"), CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE="true", CARGO_BUILD_JOBS="1", CARGO_PROFILE_DEV_DEBUG="0", CARGO_PROFILE_TEST_DEBUG="0", CARGO_INCREMENTAL="0")
        run("cargo", "test", "--locked", "--all-features", "--test", "remote_cli", "--test", "caddy_tls", "--test", "replication_drill", "--no-run", cwd=ROOT, env=environment)
        binaries = {}
        for name in ("remote_cli", "caddy_tls", "replication_drill"):
            matches = [p for p in (target / "debug/deps").glob(name + "-*") if p.is_file() and os.access(p, os.X_OK)]
            if len(matches) != 1:
                raise RuntimeError(f"expected one test executable for {name}, found {len(matches)}")
            binaries[name] = str(matches[0])
        archive = run("git", "-C", str(sibling), "archive", REPLICATION_REV, "rust", stdout=subprocess.PIPE).stdout
        source = work / "replication-source"
        source.mkdir()
        with tarfile.open(fileobj=io.BytesIO(archive)) as files:
            files.extractall(source, filter="data")
        run("cargo", "build", "--locked", "--offline", "--manifest-path", str(source / "rust/Cargo.toml"), "-p", "sqlite-replication-core", "--example", "snapshot-restore", "--target-dir", str(work / "replication"), env=environment)
        # Preserve a trusted public CA bundle for the AWS SDK's native-root setup.
        bundle = Path("/etc/ssl/certs/ca-certificates.crt").read_bytes()
        (work / "ca-bundle.crt").write_bytes(bundle)
        (work / "snapshot-restore").write_bytes((work / "replication/debug/examples/snapshot-restore").read_bytes())
        (work / "snapshot-restore").chmod(0o755)
        env_prefix = ["env", "HOME=/tmp", "TMPDIR=/tmp", "SSL_CERT_FILE=/fixture/ca-bundle.crt", "TOKIO_WORKER_THREADS=2"]
        with DockerContainer(FLOCI).with_kwargs(network_mode="none", labels={LABEL: RUN_ID}).with_env("FLOCI_STORAGE_MODE", "memory").with_env("FLOCI_DEFAULT_REGION", "us-east-1") as floci:
            with runner("container:" + floci.get_wrapped_container().id, target, work) as test:
                checked_exec(test, ["bash", "-c", "for i in $(seq 1 120); do (echo > /dev/tcp/127.0.0.1/4566) 2>/dev/null && exit 0; sleep 1; done; exit 1"])
                checked_exec(test, env_prefix + ["DISCORD_MCP_FLOCI_FIXTURE_ADDRESS=127.0.0.1:4566", "timeout", "180s", binaries["remote_cli"], "--ignored", "--test-threads=1"])
        caddy_work = work / "caddy"
        caddy_work.mkdir()
        for name in ("generated", "data", "config"):
            (caddy_work / name).mkdir()
        run(str(target / "debug/discord-mcp"), "setup", "remote", "caddy", "example.test", str(caddy_work / "generated"), "--confirm", env={"PATH": "/usr/bin:/bin", "HOME": str(work)}, stdout=subprocess.DEVNULL)
        (caddy_work / "Caddyfile").write_text((ROOT / "drills/caddy/global-options.caddy").read_text() + (caddy_work / "generated/discord-mcp.caddy").read_text())
        caddy = DockerContainer(CADDY, command="caddy run --config /fixture/Caddyfile --adapter caddyfile")
        caddy.with_kwargs(network_mode="none", labels={LABEL: RUN_ID}, read_only=True, cap_drop=["ALL"], cap_add=["NET_BIND_SERVICE"], security_opt=["no-new-privileges"])
        caddy.with_tmpfs_mount("/tmp", "rw,nosuid,nodev,size=32m")
        caddy.with_volume_mapping(str(caddy_work), "/fixture", mode="rw").with_env("XDG_DATA_HOME", "/fixture/data").with_env("XDG_CONFIG_HOME", "/fixture/config")
        with caddy:
            ca = caddy_work / "data/caddy/pki/authorities/local/root.crt"
            for _ in range(120):
                if ca.is_file() and ca.stat().st_size:
                    break
                time.sleep(0.25)
            else:
                raise RuntimeError("synthetic Caddy CA not ready")
            with runner("container:" + caddy.get_wrapped_container().id, target, work) as test:
                checked_exec(test, env_prefix + ["DISCORD_MCP_CADDY_CA=/fixture/caddy/data/caddy/pki/authorities/local/root.crt", "DISCORD_MCP_CADDY_CONFIG=/fixture/caddy/Caddyfile", "timeout", "120s", binaries["caddy_tls"], "--ignored", "--test-threads=1"])
            stdout, stderr = caddy.get_logs()
            if b"synthetic-log-marker" in stdout + stderr:
                raise RuntimeError("synthetic marker appeared in Caddy logs")
        with runner("none", target, work) as test:
            checked_exec(test, env_prefix + ["DISCORD_MCP_REPLICATION_EXAMPLE=/fixture/snapshot-restore", "timeout", "120s", binaries["replication_drill"], "--ignored", "--test-threads=1"])
    print("Containers and this run's build artifacts removed.")


if __name__ == "__main__":
    try:
        main()
    finally:
        if ROOTLESS_CONFIGURED:
            client = DockerClient().client
            try:
                for owned in client.containers.list(all=True, filters={"label": f"{LABEL}={RUN_ID}"}):
                    owned.remove(force=True, v=True)
            finally:
                client.close()
        if BUILD_PARENT_CREATED:
            try:
                (ROOT / "target").rmdir()
            except OSError:
                pass  # It may now contain another process's files.
