"""Drives an installed trill-mcp over its pipes, for scripts/update-trial.sh:
asks it which Trill it is, has it look for a newer one, fetch it, and waits
until it is ready. Nothing else: the update goes in when the window starts.

    python scripts/update-trial.py TRILL-MCP.EXE PROFILE-FOLDER THIS-VERSION NEWER-VERSION LOG

Prints what the program said, and ends with a fault where it is not what
was expected. The program is ended before this returns: it holds the lock
that says a Trill runs, and the window is to find that none does.
"""

import json
import subprocess
import sys
import time


def main() -> int:
    program, profile, this, newer, log = sys.argv[1:6]
    with open(log, "w", encoding="utf-8") as said:
        server = subprocess.Popen(
            [program, "--stdio", "--no-audio", "--no-plugins", "--profile", profile],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=said,
            text=True,
            encoding="utf-8",
        )
        try:
            return drive(server, this, newer)
        finally:
            server.stdin.close()
            try:
                server.wait(timeout=30)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


def drive(server: subprocess.Popen, this: str, newer: str) -> int:
    count = 0

    def ask(method: str, params: dict) -> dict:
        nonlocal count
        count += 1
        message = {"jsonrpc": "2.0", "id": count, "method": method, "params": params}
        server.stdin.write(json.dumps(message) + "\n")
        server.stdin.flush()
        while True:
            line = server.stdout.readline()
            if not line:
                raise SystemExit("trill-mcp ended before it answered: see its log")
            answer = json.loads(line)
            if answer.get("id") == count:
                if "error" in answer:
                    raise SystemExit(f"{method}: {answer['error']}")
                return answer["result"]

    def setup(**arguments: str) -> dict:
        result = ask("tools/call", {"name": "setup", "arguments": arguments})
        text = result["content"][0]["text"]
        if result.get("isError"):
            raise SystemExit(f"setup {arguments} was refused: {text}")
        return json.loads(text)

    ask(
        "initialize",
        {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "update-trial", "version": "1"},
        },
    )
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    server.stdin.flush()

    trill = setup(action="report")["trill"]
    print(f"    it says: {trill['this']}")
    if trill["version"] != this:
        raise SystemExit(f"the installed Trill is {trill['version']}, and {this} was expected")
    if trill.get("looks_for_newer") is False:
        raise SystemExit(f"it does not look for a newer one: {trill['why_not']}")

    trill = setup(action="look_for_newer")["trill"]
    found = trill.get("newer", {}).get("version")
    print(f"    it looked, and found: {found} ({trill.get('state')})")
    if found != newer:
        raise SystemExit(f"it found {found}, and {newer} was published: {trill}")

    began = setup(action="fetch", part="trill")
    print(f"    it is fetching: {began['fetching']}")
    since = time.monotonic()
    while True:
        trill = setup(action="report")["trill"]
        if trill.get("state") == "ready":
            print(f"    it says: {trill['ready']}")
            return 0
        if trill.get("problem") and trill.get("state") != "fetching":
            raise SystemExit(f"the fetch failed: {trill['problem']}")
        if time.monotonic() - since > 300:
            raise SystemExit(f"the newer Trill was not fetched within five minutes: {trill}")
        time.sleep(0.3)


if __name__ == "__main__":
    sys.exit(main())
