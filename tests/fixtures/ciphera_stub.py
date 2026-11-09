#!/usr/bin/env python3
"""Test-only model of the optional secret-injection command, not Ciphera."""
import json
import os
import sys
import urllib.error
import urllib.request


def main():
    args = sys.argv[1:]
    if not args or args.pop(0) != "run":
        return 2
    options = {}
    while args and args[0] != "--":
        if len(args) < 2 or args[0] not in ("--api-url", "--project", "--env"):
            return 2
        name = args.pop(0)
        options[name] = args.pop(0)
    if not args or args.pop(0) != "--" or not args:
        return 2
    origin = options.get("--api-url", os.environ.get("CIPHERA_API_URL", ""))
    if not origin.startswith("http://127.0.0.1:"):
        return 2
    request = urllib.request.Request(
        origin + "/v1/projects/" + options["--project"] + "/secrets/" + options["--env"],
        headers={"Authorization": "Bearer " + os.environ.get("CIPHERA_TOKEN", "")},
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            values = json.load(response)
    except (urllib.error.URLError, ValueError):
        return 1
    environment = os.environ.copy()
    for item in values:
        environment[item["key"]] = item["value"]
    os.execvpe(args[0], args, environment)


if __name__ == "__main__":
    sys.exit(main())
