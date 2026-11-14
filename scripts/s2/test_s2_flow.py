"""The S2 agent and sender against a stand-in Warden, without Taso.

The stand-in answers the Field the way Taso c031ee5 does for what S2
uses: a dispatching Vessel releases, a holding one answers held, a Vessel
without effect authority refuses, and the sender socket lists released
effects that are not settled.  It checks the scripts' control flow and
report files; whether Taso behaves so is what the spike runs on the
server.  Run: python3 -m unittest scripts/s2/test_s2_flow.py
"""

import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import s2_field as f  # noqa: E402


def _u8(v):
    return struct.pack("<B", v)


def _u64(v):
    return struct.pack("<Q", v)


def _b(x):
    return _u64(len(x)) + x



VESSEL = "0123456789abcdef0123456789abcdef"

class StandIn:
    """One Vessel's Field socket and sender socket."""

    def __init__(self, tmp, hold=False, authority=True, last_sealed=None):
        self.hold = hold
        self.authority = authority
        self.last_sealed = last_sealed
        self.next_op = 1
        self.released = {}  # op -> (provider, request, key, consequence, unknown)
        self.lock = threading.Lock()
        self.field_path = os.path.join(tmp, "field.sock")
        # Named like the Warden's: <vessel-id in 32 hex digits>.sock.
        self.sender_path = os.path.join(tmp, VESSEL + ".sock")
        for p in (self.field_path, self.sender_path):
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.bind(p)
            s.listen(4)
            threading.Thread(target=self._accept, args=(s,), daemon=True).start()

    def _accept(self, s):
        while True:
            c, _ = s.accept()
            threading.Thread(target=self._serve, args=(c,), daemon=True).start()

    def _serve(self, c):
        while True:
            body = f.recv_frame(c)
            if body is None:
                return
            f.send_frame(c, self._answer(body))

    def _answer(self, body):
        r = f.Reader(body)
        tag = r.u8()
        with self.lock:
            if tag == 0:
                r.u16()
                ls = self.last_sealed
                return _u8(0) + struct.pack("<H", 2) + (_u8(0) if ls is None else _u8(1) + _u64(ls))
            if tag == 1:
                call = r.u64()
                provider, request = r.string(), r.raw()
                r.raw()
                assert r.u8() == 0
                key = r.raw()
                consequence = r.consequence()
                r.raw()
                r.finish()
                if not self.authority:
                    return _u8(1) + _u64(call) + _u8(2) + _b(b"no effect authority: 1 unresolved effect(s)")
                op, self.next_op = self.next_op, self.next_op + 1
                if self.hold:
                    return _u8(1) + _u64(call) + _u8(1) + _u64(op)
                self.released[op] = (provider, request, key, consequence, False)
                if consequence == f.IRREVERSIBLE:
                    self.barrier = op
                return _u8(1) + _u64(call) + _u8(0) + _u64(op)
            if tag == 2:
                return _u8(2) + _u64(1)
            if tag == 3:
                turn = r.u64()
                self.last_sealed = turn
                return _u8(3) + _u64(turn) + _u8(1) + _u64(100 + turn)
            if tag == 4:
                op = r.u64()
                r.u8()
                if op not in self.released:
                    return _u8(5) + _u64(op) + _u8(1) + _b(b"not released")
                del self.released[op]
                self.authority = True
                return _u8(5) + _u64(op) + _u8(0)
            if tag == 5:
                out = _u8(6) + _u64(len(self.released))
                for op, (prov, req, key, cons, unknown) in sorted(self.released.items()):
                    c = {f.IRREVERSIBLE: _u8(0), f.NO_EXTERNAL_MUTATION: _u8(2)}[cons]
                    out += _u64(op) + _b(prov.encode()) + _b(req) + _u8(0) + _b(key) + c + _b(b"s2") + _u8(int(unknown))
                return out
        raise AssertionError(f"tag {tag}")

    def crash(self):
        """What a restart does to a released barrier: unknown, no authority."""
        with self.lock:
            for op, (p, q, k, c, _) in list(self.released.items()):
                self.released[op] = (p, q, k, c, True)
                if c == f.IRREVERSIBLE:
                    self.authority = False


def agent(standin, work, steps, **kw):
    cmd = [sys.executable, os.path.join(HERE, "s2-agent.py"), "--steps", ",".join(steps),
           "--socket", standin.field_path, "--workdir", work,
           "--report", os.path.join(work, "s2-report.json"), "--wait-timeout", "20"]
    return subprocess.Popen(cmd, stderr=subprocess.PIPE, text=True, **kw)


def sender(standin, *opts):
    out = subprocess.run([sys.executable, os.path.join(HERE, "s2-sender.py"),
                          "--socket", standin.sender_path, *opts],
                         check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def report(work, n_events=None, timeout=10):
    path = os.path.join(work, "s2-report.json")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if os.path.exists(path):
            with open(path) as fh:
                doc = json.load(fh)
            if n_events is None or len(doc["events"]) >= n_events:
                return doc
        time.sleep(0.02)
    raise TimeoutError(path)


class Flow(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="s2-flow-")
        self.work = os.path.join(self.tmp, "work")
        os.mkdir(self.work)

    def test_a_dispatch_then_apply(self):
        s = StandIn(self.tmp)
        p = agent(s, self.work, ["effect:nem"] * 3 + ["seal", "report", "wait:sender-done"])
        doc = report(self.work)
        self.assertEqual([e["result"] for e in doc["events"] if e["step"] == "effect"], ["Released"] * 3)
        out = sender(s, "--apply")
        self.assertEqual(len(out["before"]), 3)
        self.assertEqual(sorted(e["retry"][1] for e in out["before"]),
                         sorted(e["key"] for e in doc["events"] if e["step"] == "effect"))
        self.assertEqual(out["after"], [])
        open(os.path.join(self.work, "sender-done"), "w").close()
        self.assertEqual(p.wait(10), 0, p.stderr.read())

    def test_g_holder_answers_held(self):
        s = StandIn(self.tmp, hold=True)
        p = agent(s, self.work, ["effect:nem", "effect:irr", "seal", "report"])
        self.assertEqual(p.wait(10), 0, p.stderr.read())
        doc = report(self.work)
        self.assertEqual([e["result"] for e in doc["events"] if e["step"] == "effect"], ["Held", "Held"])
        self.assertEqual(sender(s)["before"], [])

    def test_f_lapse_restores_authority(self):
        s = StandIn(self.tmp)
        p = agent(s, self.work, ["effect:irr", "seal", "report", "stall"])
        report(self.work)
        p.kill()
        p.wait(10)
        s.crash()
        os.remove(os.path.join(self.work, "s2-report.json"))
        p = agent(s, self.work, ["effect:nem", "report", "wait:lapsed", "effect:nem", "seal", "report",
                                 "wait:done"])
        doc = report(self.work, 1)
        self.assertEqual(doc["last_sealed_at_start"], 1)
        self.assertEqual(doc["events"][0]["result"], "Refused")
        self.assertIn("no effect authority", doc["events"][0]["reason"])
        out = sender(s, "--lapse-unknown")
        self.assertEqual([(e["consequence"], e["unknown"]) for e in out["before"]], [(f.IRREVERSIBLE, True)])
        self.assertEqual([r["outcome"] for r in out["reported"]], ["NotApplied"])
        open(os.path.join(self.work, "lapsed"), "w").close()
        doc = report(self.work, 4)
        self.assertEqual([e["result"] for e in doc["events"] if e["step"] == "effect"], ["Refused", "Released"])
        out = sender(s, "--apply")
        self.assertEqual(len(out["before"]), 1)
        self.assertEqual(out["after"], [])
        open(os.path.join(self.work, "done"), "w").close()
        self.assertEqual(p.wait(10), 0, p.stderr.read())

    def test_h_socket_and_tcp(self):
        s = StandIn(self.tmp)
        llm = os.path.join(self.tmp, "rootfs", "run", "island", "llm.sock")
        echo = subprocess.Popen([sys.executable, os.path.join(HERE, "s2-echo.py"), llm],
                                stderr=subprocess.PIPE, text=True)
        try:
            for _ in range(200):
                if os.path.exists(llm):
                    break
                time.sleep(0.02)
            self.assertEqual(os.stat(llm).st_mode & 0o777, 0o666)
            missing = os.path.join(self.tmp, "nothing.sock")
            p = agent(s, self.work, [f"socket:{llm}:1048576", f"socket:{missing}:16",
                                     "tcp:127.0.0.1:1", "report"])
            self.assertEqual(p.wait(20), 0, p.stderr.read())
            ev = report(self.work)["events"]
            self.assertTrue(ev[0]["ok"], ev[0])
            self.assertEqual(ev[0]["received"], 1048576)
            self.assertFalse(ev[1]["ok"])
            self.assertIn("FileNotFoundError", ev[1]["error"])
            self.assertFalse(ev[2]["reached"])
        finally:
            echo.kill()
            echo.wait()

    def test_a_note_is_recorded(self):
        s = StandIn(self.tmp)
        p = agent(s, self.work, ["note:start-1", "effect:nem", "report"])
        self.assertEqual(p.wait(10), 0, p.stderr.read())
        ev = report(self.work)["events"]
        self.assertEqual(ev[0], {"step": "note", "text": "start-1"})
        self.assertEqual(ev[1]["result"], "Released")

    def test_unknown_step_fails_before_connecting(self):
        r = subprocess.run([sys.executable, os.path.join(HERE, "s2-agent.py"), "--steps", "effect:nem,dance",
                            "--socket", "/nonexistent"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn("unknown step", r.stderr)


if __name__ == "__main__":
    unittest.main()
