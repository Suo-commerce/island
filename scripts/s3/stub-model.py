#!/usr/bin/env python3
"""A stub OpenAI-compatible model for island-socket's tests and spike S5:
answers POST <base>/chat/completions, streamed (server-sent events) when
asked to stream, as one JSON object otherwise.  No model behind it: the
answer is filler of a chosen size, so what is measured is the path.

    stub-model.py --listen 127.0.0.1:8099 [--bytes 200000] [--pieces 400] [--delay 0.002]

The answer's content is --bytes long, split into --pieces deltas sent
--delay seconds apart.  Every request is appended to --requests (JSON
lines: path, the Authorization header, the body), when given.
"""

import argparse
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--listen", default="127.0.0.1:8099")
    p.add_argument("--bytes", type=int, default=200_000)
    p.add_argument("--pieces", type=int, default=400)
    p.add_argument("--delay", type=float, default=0.002)
    p.add_argument("--requests", default="")
    args = p.parse_args()
    host, port = args.listen.rsplit(":", 1)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("content-length", 0)))
            if args.requests:
                with open(args.requests, "a") as out:
                    out.write(json.dumps({"path": self.path, "auth": self.headers.get("authorization"),
                                          "body": body.decode("utf-8", "replace")}) + "\n")
            if not self.path.endswith("/chat/completions"):
                self.send_error(404)
                return
            try:
                request = json.loads(body)
            except ValueError:
                self.send_error(400)
                return
            model = request.get("model", "?")
            text = ("lorem ipsum " * (args.bytes // 12 + 1))[: args.bytes]
            if request.get("stream"):
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("transfer-encoding", "chunked")
                self.end_headers()
                size = max(1, len(text) // max(1, args.pieces))
                for i in range(0, len(text), size):
                    event = {"object": "chat.completion.chunk", "model": model,
                             "choices": [{"index": 0, "delta": {"content": text[i:i + size]}}]}
                    self._chunk(f"data: {json.dumps(event)}\n\n".encode())
                    time.sleep(args.delay)
                self._chunk(b"data: [DONE]\n\n")
                self._chunk(b"")
            else:
                answer = json.dumps({"object": "chat.completion", "model": model,
                                     "choices": [{"index": 0, "message": {"role": "assistant", "content": text}}],
                                     "usage": {"completion_tokens": len(text) // 4}}).encode()
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(answer)))
                self.end_headers()
                self.wfile.write(answer)

        def _chunk(self, data):
            self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            self.wfile.flush()

    server = ThreadingHTTPServer((host, int(port)), Handler)
    print(f"stub-model: listening on {args.listen}", file=sys.stderr, flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
