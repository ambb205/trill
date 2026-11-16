"""A stand-in for the model API, for watching what the unmodified CLI does.

Nothing here talks to anybody: the CLI is pointed at this server with a dummy
key and a scratch config folder, and every request it makes is written down.

  python model_stand_in.py <port> <script.json> <record-dir>

The script is a JSON list of replies for the main conversation, used in
order; each is {"blocks": [...], "input_tokens": N, "stop": "tool_use"|"end_turn"}
where a block is {"text": "..."} or {"tool": name, "input": {...}}. Requests
that are not the main conversation (no tools of ours in them) get "ok".
When the script runs out, the reply is the text "Done.".
"""

import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port, script_path, record_dir = int(sys.argv[1]), sys.argv[2], sys.argv[3]
script = json.load(open(script_path, encoding="utf-8"))
lock = threading.Lock()
state = {"main": 0, "count": 0}


def sse(handler, event, data):
    handler.wfile.write(f"event: {event}\ndata: {json.dumps(data)}\n\n".encode())
    handler.wfile.flush()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        self.record("GET", b"")
        body = b"{}"
        self.send_response(404)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def record(self, method, raw):
        with lock:
            state["count"] += 1
            number = state["count"]
        try:
            body = json.loads(raw) if raw else None
        except Exception:
            body = raw.decode("utf-8", "replace")
        headers = {k: ("<redacted>" if k.lower() in ("x-api-key", "authorization") else v) for k, v in self.headers.items()}
        with open(f"{record_dir}/req-{number:03d}.json", "w", encoding="utf-8") as out:
            json.dump({"method": method, "path": self.path, "headers": headers, "body": body, "at": time.time()}, out, indent=1)
        return number, body

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", "0")))
        number, body = self.record("POST", raw)
        if "/v1/messages" not in self.path or "count_tokens" in self.path:
            reply = b'{"input_tokens": 100}'
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)
            return

        tools = [tool.get("name", tool.get("type", "?")) for tool in (body.get("tools") or [])]
        system = json.dumps(body.get("system", ""))
        main = any(name.startswith("mcp__") for name in tools) or "MAIN-CONVERSATION" in system
        side_kind = None
        if not main:
            if any(str(name).startswith("web_search") for name in tools):
                side_kind = "web_search"
        with lock:
            if main and state["main"] < len(script):
                step = script[state["main"]]
                state["main"] += 1
            elif main:
                step = {"blocks": [{"text": "Done."}], "stop": "end_turn"}
            else:
                step = {"blocks": [{"text": "ok (side request)"}], "stop": "end_turn"}
        if step.get("delay"):
            time.sleep(step["delay"])
        if step.get("status"):
            reply = json.dumps(step.get("error", {"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})).encode()
            self.send_response(step["status"])
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)
            return

        usage = {
            "input_tokens": step.get("input_tokens", 1000),
            "output_tokens": 1,
            "cache_creation_input_tokens": step.get("cache_creation", 0),
            "cache_read_input_tokens": step.get("cache_read", 0),
        }
        model = body.get("model", "claude-test")
        stop = step.get("stop") or ("tool_use" if any("tool" in block for block in step["blocks"]) else "end_turn")
        message = {
            "id": f"msg_fake_{number:04d}", "type": "message", "role": "assistant", "model": model, "content": [],
            "stop_reason": None, "stop_sequence": None, "usage": usage,
        }
        if not body.get("stream"):
            content = []
            for index, block in enumerate(step["blocks"]):
                if "text" in block:
                    content.append({"type": "text", "text": block["text"]})
                else:
                    content.append({"type": "tool_use", "id": f"toolu_fake_{number:04d}_{index}", "name": block["tool"], "input": block["input"]})
            message.update(content=content, stop_reason=stop)
            message["usage"]["output_tokens"] = step.get("output_tokens", 50)
            reply = json.dumps(message).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)
            return

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        sse(self, "message_start", {"type": "message_start", "message": message})
        for index, block in enumerate(step["blocks"]):
            if "text" in block:
                sse(self, "content_block_start", {"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}})
                sse(self, "content_block_delta", {"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": block["text"]}})
            else:
                start = {"type": "tool_use", "id": f"toolu_fake_{number:04d}_{index}", "name": block["tool"], "input": {}}
                sse(self, "content_block_start", {"type": "content_block_start", "index": index, "content_block": start})
                sse(self, "content_block_delta", {"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}})
            sse(self, "content_block_stop", {"type": "content_block_stop", "index": index})
        sse(self, "message_delta", {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": step.get("output_tokens", 50)}})
        sse(self, "message_stop", {"type": "message_stop"})
        self.close_connection = True


server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
print(f"fake api on {port}", flush=True)
server.serve_forever()
