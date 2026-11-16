"""Turns a recording of the CLI's output into a fixture: everything particular
to the machine it was made on is removed or replaced.

  python clean_recording.py <recording.jsonl> <fixture.jsonl> <number>
"""

import json
import re
import sys

source, target, number = sys.argv[1], sys.argv[2], int(sys.argv[3])
session = f"{number:08d}-0000-4000-8000-000000000000"
uuids = {}
DROPPED = {"messaging_socket_path", "powershell_path", "original_cwd", "additional_directories"}
PATH = re.compile(r"[A-Za-z]:[\\/]+Users[\\/]+[^\"\s|]*")
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def uuid_for(old):
    if old not in uuids:
        uuids[old] = f"00000000-0000-4000-8000-{len(uuids) + 1:012d}"
    return uuids[old]


def clean(value, key=None):
    if isinstance(value, dict):
        out = {}
        for name, inner in value.items():
            if name in DROPPED:
                continue
            if name == "session_id":
                out[name] = session
            elif name == "cwd":
                out[name] = "C:/Trill/assistant"
            elif name == "apiKeySource":
                out[name] = "none"
            elif name == "timestamp":
                out[name] = "2026-11-16T12:00:00.000Z"
            elif name == "X-Claude-Code-Session-Id":
                out[name] = session
            else:
                out[name] = clean(inner, name)
        return out
    if isinstance(value, list):
        return [clean(inner, key) for inner in value]
    if isinstance(value, str):
        if key == "text" and value.startswith("This session is being continued from a previous conversation"):
            # What the CLI tells the model after summarising: it names the
            # folder it runs in. The first sentence is enough to know it by.
            return value.split("\n")[0]
        value = PATH.sub("C:/Trill/assistant", value)
        return UUID.sub(lambda match: uuid_for(match.group(0)), value)
    return value


lines = []
for line in open(source, encoding="utf-8"):
    line = line.strip()
    if not line:
        continue
    lines.append(json.dumps(clean(json.loads(line)), ensure_ascii=False, separators=(",", ":")))
text = "\n".join(lines) + "\n"
# Nothing of whoever made the recording may be left: not their name, not
# their folders, not the stand-in's key.
import os

mine = [os.environ.get(name, "") for name in ("USERNAME", "USER")]
for word in [word for word in mine if len(word) > 2] + ["AppData", "/home/", "dummy-key"]:
    assert word not in text, f"{word} is still in {target}"
open(target, "w", encoding="utf-8", newline="\n").write(text)
print(target, len(lines), "lines", len(text), "bytes")
