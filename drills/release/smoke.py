#!/usr/bin/env python3
"""Smoke-test a packaged binary with synthetic data and loopback only."""

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


def main():
    binary = str(Path(sys.argv[1]).resolve(strict=True))
    request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "release-smoke", "version": "1"},
        },
    }
    result = subprocess.run(
        [binary], input=json.dumps(request) + "\n", text=True,
        capture_output=True, env={}, timeout=20, check=True,
    )
    response = json.loads(result.stdout.splitlines()[0])
    assert response["id"] == 1 and "tools" in response["result"]["capabilities"]

    with tempfile.TemporaryDirectory(prefix="discord-mcp-release-") as directory:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        origin = f"http://127.0.0.1:{port}"
        env = {
            "DISCORD_MCP_DB": os.path.join(directory, "fixture.sqlite"),
            "DISCORD_MCP_KEY": "42" * 32,
            "DISCORD_MCP_LISTEN": f"127.0.0.1:{port}",
            "DISCORD_MCP_PUBLIC_ORIGIN": origin,
            "DISCORD_MCP_DISCORD_CLIENT_ID": "123",
            "DISCORD_MCP_DISCORD_CLIENT_SECRET": "synthetic-secret",
            "DISCORD_MCP_ADMIN_ID": "123",
            "DISCORD_MCP_CLIENT_ID": "fixture",
            "DISCORD_MCP_CLIENT_NAME": "Fixture",
            "DISCORD_MCP_CLIENT_REDIRECT": "http://127.0.0.1:8765/callback",
        }
        subprocess.run([binary, "setup", "local", "--init"], env=env,
                       capture_output=True, check=True, timeout=20)
        server = subprocess.Popen([binary, "serve"], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError("server exited before readiness")
                try:
                    with opener.open(origin + "/admin", timeout=1) as response:
                        assert response.status == 200
                    break
                except urllib.error.HTTPError as error:
                    if error.code in (302, 303, 401, 403):
                        break
                    raise
                except urllib.error.URLError:
                    time.sleep(0.05)
            else:
                raise RuntimeError("server did not become ready")
            try:
                opener.open(origin + "/mcp", timeout=2)
                raise AssertionError("unauthenticated MCP request was accepted")
            except urllib.error.HTTPError as error:
                assert error.code in (401, 403)
        finally:
            server.terminate()
            server.wait(timeout=10)
    print("Release binary: stdio initialization and unauthenticated HTTP checks passed")


if __name__ == "__main__":
    main()
