#!/usr/bin/env python3
"""Spike S5's agent: thinking through the island socket, from a prime and
from its fork child, inside Taso Vessels.

    s5-agent.py [--model NAME] [--island /run/island/island.sock]

It asks `GET /whoami`, then streams one chat completion through
`POST /v1/chat/completions`, timing the first byte and the whole, and
keeps the exact request and response bodies in /work (s5-request.json,
s5-response.bin) so the host can compute the call's digest and match it
with the `access` entry.  It writes /work/s5-report.json and ends its
turn.  The prime then asks Taso for a fork (the child runs this same
program and learns from whoami that it is the fork) and waits to be
killed; the child exits.
"""

import argparse
import http.client
import json
import os
import socket
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import s2_field as f  # noqa: E402


class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("island", timeout=600)
        self.unix = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(600)
        self.sock.connect(self.unix)


def log(msg):
    print(f"s5-agent: {msg}", file=sys.stderr, flush=True)


def main():
    p = argparse.ArgumentParser(description="spike S5 agent (inside a Vessel)")
    p.add_argument("--island", default="/run/island/island.sock")
    p.add_argument("--field", default="/run/taso/field.sock")
    p.add_argument("--workdir", default="/work")
    p.add_argument("--model", default="")
    args = p.parse_args()
    report = {"events": []}
    path = os.path.join(args.workdir, "s5-report.json")

    def save():
        with open(path + ".tmp", "w") as out:
            json.dump(report, out, indent=1)
        os.replace(path + ".tmp", path)

    c = UnixHTTP(args.island)
    c.request("GET", "/whoami")
    r = c.getresponse()
    me = json.loads(r.read())
    c.close()
    report["whoami"] = {"status": r.status, **me}
    save()
    log(f"whoami: {me}")

    request = {"stream": True, "messages": [
        {"role": "system", "content": "You are flare-agent, Island's agent."},
        {"role": "user", "content": f"Hello from {me.get('subject')}. " + "Context. " * 2000}]}
    if args.model:
        request["model"] = args.model
    body = json.dumps(request).encode()
    with open(os.path.join(args.workdir, "s5-request.json"), "wb") as out:
        out.write(body)
    t0 = time.monotonic()
    c = UnixHTTP(args.island)
    c.request("POST", "/v1/chat/completions", body=body, headers={"content-type": "application/json"})
    r = c.getresponse()
    first = None
    pieces, received = 0, bytearray()
    while True:
        chunk = r.read1(65536)
        if not chunk:
            break
        if first is None:
            first = time.monotonic() - t0
        pieces += 1
        received.extend(chunk)
    total = time.monotonic() - t0
    c.close()
    with open(os.path.join(args.workdir, "s5-response.bin"), "wb") as out:
        out.write(bytes(received))
    report["think"] = {"status": r.status, "provider": r.getheader("x-island-provider"),
                       "request_bytes": len(body), "response_bytes": len(received),
                       "reads": pieces, "events": received.count(b"data: "),
                       "done": received.endswith(b"data: [DONE]\n\n"),
                       "first_byte_s": round(first or 0, 4), "total_s": round(total, 4)}
    save()
    log(f"think: {report['think']}")

    field = f.Field(args.field)
    field.ask(f.transcript(b"s5: thought once"))
    sealed = field.ask(f.turn_end(1, True))
    report["sealed"] = sealed.get("sealed")
    save()
    if me.get("role") != "prime":
        field.close()
        return
    for _ in range(30):
        fork = field.ask(f.fork())
        if fork.get("result") == "Forked":
            break
        time.sleep(1)
    report["fork"] = fork
    save()
    log(f"fork: {fork}; waiting to be killed")
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
