#!/usr/bin/env python3
"""Exercise the actual Compose file using only synthetic local configuration."""

import os
import http.client
import socket
import subprocess
import time
import urllib.error
import urllib.request
import uuid


def main():
    project = "discord-mcp-fixture-" + uuid.uuid4().hex[:12]
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = {key: os.environ[key] for key in ("HOME", "PATH", "DOCKER_CONTEXT") if key in os.environ}
    env.update({
        "DISCORD_MCP_IMAGE": os.environ["DISCORD_MCP_IMAGE"],
        "DISCORD_MCP_RELAY_IMAGE": os.environ["DISCORD_MCP_RELAY_IMAGE"],
        "DISCORD_MCP_HOST_PORT": str(port),
        "DISCORD_MCP_KEY": "42" * 32,
        "DISCORD_MCP_PUBLIC_ORIGIN": "https://fixture.invalid",
        "DISCORD_MCP_DISCORD_CLIENT_ID": "123",
        "DISCORD_MCP_DISCORD_CLIENT_SECRET": "synthetic-secret",
        "DISCORD_MCP_ADMIN_ID": "123",
        "DISCORD_MCP_CLIENT_ID": "fixture",
        "DISCORD_MCP_CLIENT_NAME": "Fixture",
        "DISCORD_MCP_CLIENT_REDIRECT": "http://127.0.0.1:8765/callback",
    })
    compose = ["docker", "compose", "-p", project, "-f", "compose.yaml"]
    try:
        subprocess.run(compose + ["up", "-d", "--pull", "never"], env=env,
                       check=True, capture_output=True, timeout=90)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        request = urllib.request.Request(f"http://127.0.0.1:{port}/mcp", headers={"Host": "fixture.invalid"})
        original_id = subprocess.check_output(
            compose + ["ps", "-q", "discord-mcp"], env=env, text=True, timeout=20
        ).strip()
        if not original_id:
            raise RuntimeError("Compose did not report a server container")
        for phase in ("startup", "recreate"):
            if phase == "recreate":
                subprocess.run(compose + ["stop"], env=env, check=True,
                               capture_output=True, timeout=60)
                subprocess.run(
                    compose + ["run", "--rm", "--no-deps", "-T", "discord-mcp", "add-channel", "release-fixture"],
                    input="https://discord.com/api/webhooks/123/synthetic\n", text=True,
                    env=env, check=True, capture_output=True, timeout=60,
                )
                subprocess.run(compose + ["up", "-d", "--pull", "never", "--force-recreate"],
                               env=env, check=True, capture_output=True, timeout=90)
                recreated_id = subprocess.check_output(
                    compose + ["ps", "-q", "discord-mcp"], env=env, text=True, timeout=20
                ).strip()
                if not recreated_id or recreated_id == original_id:
                    raise RuntimeError("server container was not recreated")
            for _ in range(100):
                try:
                    opener.open(request, timeout=1)
                    raise AssertionError("unauthenticated MCP request was accepted")
                except urllib.error.HTTPError as error:
                    if error.code in (401, 403):
                        print(f"Compose {phase}: host loopback reached MCP; unauthenticated request rejected")
                        break
                    if error.code == 502:
                        time.sleep(0.1)
                        continue
                    raise
                except (urllib.error.URLError, http.client.RemoteDisconnected, ConnectionResetError):
                    time.sleep(0.1)
            else:
                raise RuntimeError(f"Compose listener did not become ready after {phase}")
        subprocess.run(compose + ["stop"], env=env, check=True,
                       capture_output=True, timeout=60)
        result = subprocess.run(
            compose + ["run", "--rm", "--no-deps", "-T", "discord-mcp", "list-channels"],
            env=env, check=True, capture_output=True, text=True, timeout=60,
        )
        if "release-fixture" not in result.stdout.splitlines():
            raise RuntimeError("synthetic channel missing after container recreation")
        print("Compose persistence: synthetic SQLite channel survived server container recreation")
    finally:
        subprocess.run(compose + ["down"], env=env, capture_output=True, timeout=60)
        print(f"Fixture volume retained: {project}_discord-mcp-data")


if __name__ == "__main__":
    main()
