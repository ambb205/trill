"""A minimal MCP server over HTTP with one slow tool, to find the CLI's limit
on how long a tool call may take. Also writes down every request it gets,
with its headers, so that what a web tool sends to a local address can be
read.

  python waiting_mcp_server.py <port> <record-file>
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port, record = int(sys.argv[1]), sys.argv[2]


def note(entry):
    with open(record, "a", encoding="utf-8") as out:
        out.write(json.dumps(entry) + "\n")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def answer(self, status, body, kind="application/json"):
        raw = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        note({"method": "GET", "path": self.path, "headers": dict(self.headers.items()), "at": time.time()})
        if self.path.startswith("/page"):
            self.answer(200, b"<html><body><h1>Local page</h1><p>SECRET-LOCAL-TEXT-4471</p></body></html>", "text/html")
        else:
            self.answer(405, {"error": "method not allowed"})

    def do_DELETE(self):
        note({"method": "DELETE", "path": self.path, "at": time.time()})
        self.answer(200, {})

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", "0")))
        try:
            message = json.loads(raw)
        except Exception:
            message = {}
        note({"method": "POST", "path": self.path, "headers": dict(self.headers.items()), "rpc": message.get("method"), "at": time.time()})
        method, identifier = message.get("method"), message.get("id")
        if identifier is None:
            self.answer(202, b"")
            return
        if method == "initialize":
            result = {
                "protocolVersion": message["params"].get("protocolVersion", "2025-03-26"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "slow", "version": "0"},
            }
        elif method == "tools/list":
            result = {"tools": [{
                "name": "wait",
                "description": "Waits the given number of seconds, then answers.",
                "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number"}}, "required": ["seconds"]},
            }]}
        elif method == "tools/call":
            seconds = float(message["params"]["arguments"].get("seconds", 1))
            started = time.time()
            time.sleep(seconds)
            note({"finished_wait": seconds, "took": time.time() - started})
            result = {"content": [{"type": "text", "text": f"waited {seconds} s"}]}
        else:
            self.answer(200, {"jsonrpc": "2.0", "id": identifier, "error": {"code": -32601, "message": "no such method"}})
            return
        try:
            self.answer(200, {"jsonrpc": "2.0", "id": identifier, "result": result})
        except Exception as error:
            note({"client_gone": str(error)})


ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
