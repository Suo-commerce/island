"""Spike S4's agent and host socket against a stand-in Warden, without Taso.

The stand-in answers Hello, Transcript, TurnEnd, Effect, Fork, Promote and
Result.  A promotion is played by hand the way Taso does it: the run ends,
and the next start finds the child's files in /work.  This checks the
agent's plan-following and resuming; what Taso really does is the spike.
Run: python3 -m unittest scripts/s2/test_s4_agent.py
"""

import json
import os
import shutil
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


class Warden:
    """A Field socket that keeps its last sealed turn across connections."""

    def __init__(self, path, hold=False):
        self.hold = hold
        self.last_sealed = None
        self.next_op = 1
        self.states = {}
        self.forks = 0
        self.lock = threading.Lock()
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.bind(path)
        s.listen(4)
        threading.Thread(target=self._accept, args=(s,), daemon=True).start()

    def _accept(self, s):
        while True:
            c, _ = s.accept()
            threading.Thread(target=self._serve, args=(c,), daemon=True).start()

    def _serve(self, c):
        try:
            while True:
                body = f.recv_frame(c)
                if body is None:
                    return
                f.send_frame(c, self._answer(body))
        except OSError:
            return

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
                r.string(), r.raw(), r.raw()
                r.u8(), r.raw()
                r.consequence()
                r.raw()
                op, self.next_op = self.next_op, self.next_op + 1
                self.states[op] = "Held" if self.hold else "Pending"
                return _u8(1) + _u64(call) + _u8(1 if self.hold else 0) + _u64(op)
            if tag == 2:
                return _u8(2) + _u64(1)
            if tag == 3:
                turn = r.u64()
                self.last_sealed = turn
                return _u8(3) + _u64(turn) + _u8(1) + _u64(100 + turn)
            if tag == 6:
                self.forks += 1
                return _u8(7) + _u8(0) + _b(f"child-{self.forks}".encode()) + _u64(100 + (self.last_sealed or 0))
            if tag == 7:
                child = r.string()
                return _u8(8) + _b(child.encode()) + _u8(0) + _u64(7) + _u64(1) + _u64(self.next_op)
            if tag == 8:
                op = r.u64()
                state = {"Pending": 0, "Held": 1}.get(self.states.get(op), 5)
                return _u8(9) + _u64(op) + _u8(state)
        raise AssertionError(f"tag {tag}")


PRIME = ",".join([
    "mark:prime-start", "bulk:20:1", "seal", "touch:3", "seal", "mark:fork-task", "seal",
    "fork", "rm:fork-task", "mark:prime-after-fork", "seal", "waitnote:1",
    "mark:prime-unsealed", "effect:irr:unended", "promote",
    "ls", "result:unended", "effect:irr:after", "fork", "seal", "idle",
])
FORK = ",".join([
    "ls", "rm:fork-task", "mark:child-made", "effect:irr:child-a", "setprime:15", "seal",
    "note:done", "exit",
])


class S4(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="s4-agent-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.journal = os.path.join(self.tmp, "journal.jsonl")
        self.island = os.path.join(self.tmp, "island.sock")
        self.procs = []

    def tearDown(self):
        for p in self.procs:
            p.kill()
            p.wait(5)
            if p.stderr:
                p.stderr.close()

    def host(self, slot):
        """The host socket, with the uid base set so this process is in `slot`."""
        if os.path.exists(self.island):
            os.unlink(self.island)
        p = subprocess.Popen([sys.executable, os.path.join(HERE, "s4-host.py"), "--socket", self.island,
                              "--journal", self.journal, "--prime-slot", "10",
                              "--uid-base", str(os.getuid() - slot * 65536)],
                             stderr=subprocess.DEVNULL)
        self.procs.append(p)
        for _ in range(100):
            if os.path.exists(self.island):
                return p
            time.sleep(0.05)
        raise RuntimeError("s4-host did not start")

    def agent(self, field, work, timeout=None, **kw):
        p = subprocess.Popen([sys.executable, os.path.join(HERE, "s4-agent.py"), "--prime", PRIME,
                              "--fork", FORK, "--socket", field, "--island", self.island,
                              "--workdir", work, "--wait-timeout", "10", "--promote-wait", "0.3"],
                             stderr=subprocess.PIPE, text=True)
        self.procs.append(p)
        return p

    def events(self, **match):
        if not os.path.exists(self.journal):
            return []
        with open(self.journal) as fh:
            out = [json.loads(line) for line in fh]
        return [e for e in out if all(e.get(k) == v for k, v in match.items())]

    def wait_for(self, timeout=10, **match):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            got = self.events(**match)
            if got:
                return got
            time.sleep(0.05)
        self.fail(f"no journal event {match}: {self.events()}")

    def test_prime_child_promotion_and_resume(self):
        prime_work = os.path.join(self.tmp, "prime")
        os.mkdir(prime_work)
        field = os.path.join(self.tmp, "field.sock")
        Warden(field)

        # The prime runs up to its wait for a note.
        host = self.host(10)
        p = self.agent(field, prime_work)
        self.wait_for(step="fork", result="Forked")
        # The child forks from the Seal before the fork: the prime's files then.
        child_work = os.path.join(self.tmp, "child")
        shutil.copytree(prime_work, child_work)
        os.remove(os.path.join(child_work, "prime-after-fork"))
        os.makedirs(os.path.join(child_work, ".s4"), exist_ok=True)
        with open(os.path.join(child_work, ".s4", "prime.next"), "w") as fh:
            fh.write("6")  # as the fork Seal held it
        with open(os.path.join(child_work, "fork-task"), "w") as fh:
            fh.write("{}")

        # The child: another slot, a holding Warden.
        child_field = os.path.join(self.tmp, "child-field.sock")
        Warden(child_field, hold=True)
        host.kill()
        host.wait(5)
        self.host(11)
        c = self.agent(child_field, child_work)
        self.assertEqual(c.wait(10), 0, c.stderr.read())
        child = self.events(role="fork")
        self.assertEqual(child[0]["step"], "start")
        self.assertIn("fork-task", child[0]["files"])
        self.assertEqual([e["result"] for e in child if e["step"] == "effect"], ["Held"])
        self.assertTrue(self.events(step="note-posted", slot=11))

        # Back to the prime: it sees the note (the host restarted, so post
        # it again), declares, promotes, and its run ends.
        self.host(10)
        with socket.socket(socket.AF_UNIX) as s:
            s.connect(self.island)
            s.sendall(b'{"op": "note", "text": "done"}\n')
            s.recv(100)
        self.assertEqual(p.wait(10), 0, p.stderr.read())
        promote = self.wait_for(step="promote")[0]
        self.assertEqual((promote["result"], promote["child"]), ("Promoted", "child-1"))
        unended = [e for e in self.events(step="effect", tag="unended")]
        self.assertEqual(unended[0]["result"], "Released")

        # The restart on the child's files: the prime goes on at step 15.
        shutil.rmtree(prime_work)
        shutil.copytree(child_work, prime_work)
        p2 = self.agent(field, prime_work)
        idle = self.wait_for(step="idle")
        self.assertEqual(idle[0]["role"], "prime")
        start = [e for e in self.events(step="start", role="prime")][-1]
        self.assertEqual(start["resume_at"], 15)
        ls = self.events(step="ls", role="prime")[-1]["files"]
        self.assertIn("child-made", ls)
        self.assertNotIn("fork-task", ls)
        self.assertNotIn("prime-unsealed", ls)
        result = self.events(step="result", tag="unended")[-1]
        self.assertEqual(result["state"], "Pending")
        self.assertEqual(self.events(step="fork")[-1]["child"], "child-2")
        p2.kill()

    def test_a_crash_resumes_after_the_last_seal(self):
        work = os.path.join(self.tmp, "w")
        os.mkdir(work)
        field = os.path.join(self.tmp, "field.sock")
        Warden(field)
        self.host(10)
        p = self.agent(field, work)
        self.wait_for(step="fork", result="Forked")
        p.kill()
        p.wait(5)
        # The files roll back to the Seal before the fork: the fork-task Seal.
        with open(os.path.join(work, ".s4", "prime.next"), "w") as fh:
            fh.write("7")
        p = self.agent(field, work)
        starts = self.wait_for(step="start", resume_at=7)
        self.assertEqual(starts[0]["role"], "prime")
        self.wait_for(step="fork", child="child-2")
        p.kill()


if __name__ == "__main__":
    unittest.main()
