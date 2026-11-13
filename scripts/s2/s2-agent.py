#!/usr/bin/env python3
"""The S2 spike's agent: runs inside a Taso Vessel and does what its
steps say, recording each result in /work/s2-report.json.

    s2-agent.py --steps STEP,STEP,...

Steps:
    effect:nem     declare an effect with consequence NoExternalMutation
    effect:irr     declare an Irreversible effect (a barrier: if its outcome
                   is lost, the Vessel loses effect authority)
    seal           Transcript + TurnEnd: the turn is sealed
    report         write the results so far to the report file (atomically)
    wait:NAME      wait until /work/NAME exists (the host's go-ahead)
    stall          sleep until killed (a turn that never ends)
    socket:PATH:N  connect to the Unix socket PATH, send N random bytes,
                   expect them echoed back whole (the inference-socket test)
    tcp:HOST:PORT  try a TCP connection; recorded as reached or refused
                   (the Vessel has no network, so refused is expected)
    note:TEXT      record TEXT (the host names each start with one, so it
                   can tell this start's report from one left before it)

Each effect gets a fresh 16-byte idempotency key, so the host can tell
which declaration reached which sender.  The agent never lists or
reports outcomes: that is the sender's job, outside the Vessel.
"""

import argparse
import hashlib
import json
import os
import socket
import struct
import sys
import threading
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import s2_field as f  # noqa: E402


def log(msg):
    print(f"s2-agent: {msg}", file=sys.stderr, flush=True)


class Agent:
    def __init__(self, args):
        self.args = args
        self.field = f.Field(args.socket)
        self.last_sealed = self.field.hello["last_sealed_turn"]
        self.turn = (self.last_sealed or 0) + 1
        self.index = 0  # effects declared in this turn
        self.events = []
        log(f"hello: last sealed turn {self.last_sealed}, now turn {self.turn}")

    # ── steps ────────────────────────────────────────────────────────

    def effect(self, kind):
        consequence = {"nem": f.NO_EXTERNAL_MUTATION, "irr": f.IRREVERSIBLE}[kind]
        call = self.turn * 1000 + self.index
        self.index += 1
        key = uuid.uuid4().bytes
        request = f"s2 turn {self.turn} effect {self.index} ({kind})".encode()
        reply = self.field.ask(f.effect(call, self.args.provider, request, key, consequence))
        if reply.get("type") != "Effect" or reply.get("call") != call:
            raise RuntimeError(f"unexpected answer to effect {call}: {reply}")
        ev = {"step": "effect", "turn": self.turn, "call": call, "consequence": consequence,
              "key": key.hex(), "result": reply["result"]}
        if "op" in reply:
            ev["op"] = reply["op"]
        if "reason" in reply:
            ev["reason"] = reply["reason"]
        self.events.append(ev)
        log(f"effect call={call} {consequence} -> {reply['result']} {reply.get('op', reply.get('reason', ''))}")

    def seal(self):
        r = self.field.ask(f.transcript(f"s2: turn {self.turn}, {self.index} effect(s)".encode()))
        if r.get("type") != "Recorded":
            raise RuntimeError(f"unexpected answer to the transcript: {r}")
        r = self.field.ask(f.turn_end(self.turn, True))
        if r.get("type") != "TurnEnd":
            raise RuntimeError(f"unexpected answer to TurnEnd: {r}")
        self.events.append({"step": "seal", "turn": self.turn, "sealed": r["sealed"]})
        log(f"turn {self.turn} sealed at {r['sealed']}")
        self.turn += 1
        self.index = 0

    def report(self):
        doc = {"last_sealed_at_start": self.last_sealed, "events": self.events}
        tmp = self.args.report + ".tmp"
        with open(tmp, "w") as out:
            json.dump(doc, out, indent=1)
        os.replace(tmp, self.args.report)

    def wait(self, name):
        path = os.path.join(self.args.workdir, name)
        deadline = time.monotonic() + self.args.wait_timeout
        log(f"waiting for {path}")
        while not os.path.exists(path):
            if time.monotonic() > deadline:
                raise TimeoutError(f"gave up waiting for {path}")
            time.sleep(0.05)
        self.events.append({"step": "wait", "name": name})

    def stall(self):
        log("stalling: this turn never ends")
        while True:
            time.sleep(3600)

    def socket_echo(self, path, n):
        ev = {"step": "socket", "path": path, "bytes": n}
        payload = os.urandom(n)
        t0 = time.monotonic()
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(30)
            s.connect(path)
            # Send from a thread: the echo comes back while the rest is
            # still going out, and both directions' buffers are finite.
            out = threading.Thread(target=s.sendall, args=(struct.pack("<Q", n) + payload,),
                                   daemon=True)
            out.start()
            got = bytearray()
            while len(got) < n:
                chunk = s.recv(min(1 << 16, n - len(got)))
                if not chunk:
                    break
                got.extend(chunk)
            out.join(5)
            s.close()
            ev["ok"] = hashlib.sha256(got).digest() == hashlib.sha256(payload).digest()
            ev["received"] = len(got)
        except OSError as e:
            ev["ok"] = False
            ev["error"] = f"{type(e).__name__}: {e}"
        ev["seconds"] = round(time.monotonic() - t0, 4)
        self.events.append(ev)
        log(f"socket {path}: {ev}")

    def tcp(self, host, port):
        ev = {"step": "tcp", "host": host, "port": port}
        try:
            s = socket.create_connection((host, port), timeout=3)
            s.close()
            ev["reached"] = True
        except OSError as e:
            ev["reached"] = False
            ev["error"] = f"{type(e).__name__}: {e}"
        self.events.append(ev)
        log(f"tcp {host}:{port}: {ev}")

    # ── driver ───────────────────────────────────────────────────────

    def run(self, steps):
        for step in steps:
            name, _, rest = step.partition(":")
            if name == "effect":
                self.effect(rest)
            elif name == "seal":
                self.seal()
            elif name == "report":
                self.report()
            elif name == "wait":
                self.wait(rest)
            elif name == "stall":
                self.stall()
            elif name == "socket":
                path, _, n = rest.rpartition(":")
                self.socket_echo(path, int(n))
            elif name == "note":
                self.events.append({"step": "note", "text": rest})
            elif name == "tcp":
                host, _, port = rest.rpartition(":")
                self.tcp(host, int(port))
            else:
                raise ValueError(f"unknown step {step!r}")
        self.field.close()


def main():
    p = argparse.ArgumentParser(description="S2 spike agent (inside a Vessel)")
    p.add_argument("--steps", required=True, help="comma-separated steps")
    p.add_argument("--socket", default="/run/taso/field.sock")
    p.add_argument("--provider", default="island")
    p.add_argument("--workdir", default="/work")
    p.add_argument("--report", default="/work/s2-report.json")
    p.add_argument("--wait-timeout", type=float, default=1800.0)
    args = p.parse_args()
    steps = [s for s in args.steps.split(",") if s]
    # Validate before connecting, so a typo fails before anything is declared.
    known = {"effect", "seal", "report", "wait", "stall", "socket", "tcp", "note"}
    for s in steps:
        if s.partition(":")[0] not in known:
            p.error(f"unknown step {s!r}")
    Agent(args).run(steps)


if __name__ == "__main__":
    main()
